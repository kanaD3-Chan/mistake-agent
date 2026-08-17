//! Tauri GUI 入口：kernel 直接运行在本进程内（standalone，无 sidecar 依赖）。
//! GUI ↔ kernel 经内存通道桥接：前端请求 → Kernel::handle → 响应/事件帧 → Channel 回推。

// Release 构建在 Windows 下不弹出黑色控制台窗口（双击 .exe 直接进 GUI）。
// 非 Windows 平台该属性不存在，cfg_attr 让它完全跳过。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use serde::Serialize;
use tauri::{Manager, State, ipc::Channel};
use tokio::sync::mpsc;

use mistake_agent::kernel::agent::rpc::{Kernel, RpcFrame, RpcRequest};
use mistake_agent::kernel::events::{Event, EventSink};

/// 进程内桥接：前端 → kernel 的请求通道 + kernel 句柄。
struct KernelBridge {
    req_tx: mpsc::UnboundedSender<String>,
    #[allow(dead_code)]
    kernel: Arc<Kernel>,
}

/// kernel 事件 → 前端 Channel（事件帧 JSONL，与 RPC 响应共用通道）。
struct ChannelEventSink {
    on_frame: Channel<String>,
}

impl EventSink for ChannelEventSink {
    fn emit(&self, event: Event) {
        let frame = RpcFrame::Event { event };
        if let Ok(line) = serde_json::to_string(&frame) {
            let _ = self.on_frame.send(line);
        }
    }
}

/// 启动进程内 kernel：Kernel::new 完成后，请求循环在 Tauri async runtime 上运行。
#[tauri::command]
async fn start_kernel(app: tauri::AppHandle, on_frame: Channel<String>) -> Result<(), String> {
    let events: Arc<dyn EventSink> = Arc::new(ChannelEventSink {
        on_frame: on_frame.clone(),
    });
    let kernel = Kernel::new(events).await?;

    let (req_tx, mut req_rx) = mpsc::unbounded_channel::<String>();
    let kernel_for_loop = kernel.clone();
    let on_frame_for_loop = on_frame.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(line) = req_rx.recv().await {
            let Ok(request) = serde_json::from_str::<RpcRequest>(&line) else {
                continue;
            };
            let id = request.id;
            let frame = match kernel_for_loop.handle(request).await {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
                Err(e) => RpcFrame::Response {
                    id,
                    result: None,
                    error: Some(e),
                },
            };
            if let Ok(line) = serde_json::to_string(&frame) {
                let _ = on_frame_for_loop.send(line);
            }
        }
    });

    app.manage(KernelBridge { req_tx, kernel });
    Ok(())
}

/// 前端请求 → kernel（一行 JSONL，协议帧格式与早期 sidecar 时代一致，前端零改动）。
#[tauri::command]
fn kernel_send(state: State<'_, KernelBridge>, line: String) -> Result<(), String> {
    state.req_tx.send(line).map_err(|e| e.to_string())
}

/// 上传结果：temp_path 给 kernel（安全暂存，处理后删除）；
/// asset_path 是数据根目录 uploads/ 的持久副本，供前端展示（不随 temp 删除）。
#[derive(Serialize)]
struct PickResult {
    temp_path: String,
    asset_path: String,
    name: String,
}

/// 作业文件选择器：所选文件同时生成两份副本——
/// 1) 系统临时目录（mistake-agent- 前缀，kernel 白名单，处理后即删）；
/// 2) 数据根目录 uploads/（持久化，前端图片/PDF 展示用）。
#[tauri::command]
async fn pick_homework_file() -> Result<Option<PickResult>, String> {
    // 异步文件对话框：阻塞式 pick_file() 在 Linux 上需要主线程/走 portal，
    // 放在命令线程会挂起导致界面卡死；AsyncFileDialog 在后台线程弹窗（rfd 0.17）。
    let picked = rfd::AsyncFileDialog::new()
        .add_filter("作业文件", &["png", "jpg", "jpeg", "webp", "bmp", "pdf"])
        .pick_file()
        .await;
    picked.map(|p| stage_files(p.path())).transpose()
}

/// 复制到系统临时目录，文件名带 mistake-agent- 前缀（kernel 白名单依据）。
fn stage_files(source: &Path) -> Result<PickResult, String> {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("dat")
        .to_string();
    let bytes = std::fs::read(source).map_err(|e| format!("读取文件失败：{e}"))?;
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("附件")
        .to_string();
    stage_bytes(&bytes, &ext, name)
}

/// 把附件字节写入两份副本——1) 系统临时目录（mistake-agent- 前缀，kernel 白名单，处理后即删）；
/// 2) 数据根目录 uploads/（持久化，前端图片/PDF 展示用）。「选择作业文件」与「剪贴板粘贴」共用。
fn stage_bytes(bytes: &[u8], ext: &str, name: String) -> Result<PickResult, String> {
    let uuid = uuid::Uuid::new_v4();
    let temp_name = format!("mistake-agent-{uuid}.{ext}");
    let temp_dest = std::env::temp_dir().join(&temp_name);
    std::fs::write(&temp_dest, bytes).map_err(|e| format!("暂存文件失败：{e}"))?;

    let root = mistake_agent::kernel::settings::Settings::data_root();
    let uploads = root.join("uploads");
    std::fs::create_dir_all(&uploads).map_err(|e| format!("创建附件目录失败：{e}"))?;
    let asset_name = format!("{uuid}.{ext}");
    let asset_dest = uploads.join(&asset_name);
    std::fs::write(&asset_dest, bytes).map_err(|e| format!("附件持久化失败：{e}"))?;

    Ok(PickResult {
        temp_path: temp_dest.to_string_lossy().into_owned(),
        asset_path: asset_dest.to_string_lossy().into_owned(),
        name,
    })
}

/// 剪贴板粘贴截图：接收图片字节（base64 + MIME），走与「选择作业文件」相同的暂存管线。
#[tauri::command]
fn stage_clipboard_image(mime: String, data_base64: String) -> Result<PickResult, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_base64.trim())
        .map_err(|e| format!("图片数据解码失败：{e}"))?;
    let ext = ext_for_mime(&mime);
    stage_bytes(&bytes, ext, format!("粘贴截图.{ext}"))
}

/// 剪贴板图片 MIME → 文件扩展名（截图几乎总是 png，未知类型兜底 png）。
fn ext_for_mime(mime: &str) -> &'static str {
    let mime = mime.trim().to_ascii_lowercase();
    match mime.as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "png",
    }
}

/// 读取 uploads/ 持久附件（base64），前端渲染图片/PDF 用。
/// 安全白名单：只允许数据根目录 uploads/ 下的文件（canonicalize 防符号链接逃逸）。
#[tauri::command]
fn read_upload(path: String) -> Result<String, String> {
    let uploads = mistake_agent::kernel::settings::Settings::data_root().join("uploads");
    let canonical = verify_in_uploads(&path, &uploads)?;
    use base64::Engine;
    let bytes = std::fs::read(&canonical).map_err(|e| format!("读取附件失败：{e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// 用系统默认程序打开附件（PDF 预览兜底：WebView 打不开时学生也能看）。
#[tauri::command]
fn open_attachment(path: String) -> Result<(), String> {
    let uploads = mistake_agent::kernel::settings::Settings::data_root().join("uploads");
    let canonical = verify_in_uploads(&path, &uploads)?;
    open_with_system(&canonical)
}

/// 用系统默认程序打开教学规则文件（数据根 AGENTS.md，家长/老师编辑用）。
/// 路径固定为数据根目录下的 AGENTS.md（bootstrap 已保证存在），不接收用户输入路径。
#[tauri::command]
fn open_rules_file() -> Result<(), String> {
    let path = mistake_agent::kernel::settings::Settings::data_root().join("AGENTS.md");
    if !path.is_file() {
        return Err("教学规则文件不存在，请先完成首次初始化".into());
    }
    open_with_system(&path)
}

/// 用系统默认程序打开文件（跨平台）。
fn open_with_system(target: &std::path::Path) -> Result<(), String> {
    let target = target.to_string_lossy().into_owned();
    #[cfg(target_os = "windows")]
    let status = Command::new("cmd")
        .args(["/C", "start", "", &target])
        .spawn()
        .map_err(|e| format!("打开文件失败：{e}"))?
        .wait();
    #[cfg(target_os = "macos")]
    let status = Command::new("open")
        .arg(&target)
        .spawn()
        .map_err(|e| format!("打开文件失败：{e}"))?
        .wait();
    #[cfg(target_os = "linux")]
    let status = Command::new("xdg-open")
        .arg(&target)
        .spawn()
        .map_err(|e| format!("打开文件失败：{e}"))?
        .wait();
    status.map_err(|e| format!("打开文件失败：{e}"))?;
    Ok(())
}

/// 附件白名单校验：路径必须位于 uploads/ 目录内（canonicalize 防符号链接逃逸）。
fn verify_in_uploads(path: &str, uploads: &Path) -> Result<PathBuf, String> {
    let canonical = Path::new(path)
        .canonicalize()
        .map_err(|e| format!("无法读取附件：{e}"))?;
    let uploads_canon = uploads
        .canonicalize()
        .map_err(|_| "附件目录不可用".to_string())?;
    if !canonical.starts_with(&uploads_canon) {
        return Err("附件路径不在允许目录内".into());
    }
    Ok(canonical)
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            start_kernel,
            kernel_send,
            pick_homework_file,
            stage_clipboard_image,
            read_upload,
            open_attachment,
            open_rules_file
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 应用运行失败");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_whitelist_accepts_only_uploads_dir() {
        let tmp = std::env::temp_dir().join(format!("ma-upload-test-{}", uuid::Uuid::new_v4()));
        let uploads = tmp.join("uploads");
        std::fs::create_dir_all(&uploads).unwrap();
        let inside = uploads.join("a.png");
        std::fs::write(&inside, b"x").unwrap();
        let outside = tmp.join("secret.txt");
        std::fs::write(&outside, b"x").unwrap();
        assert!(verify_in_uploads(&inside.to_string_lossy(), &uploads).is_ok());
        assert!(verify_in_uploads(&outside.to_string_lossy(), &uploads).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ext_for_mime_maps_common_types() {
        assert_eq!(ext_for_mime("image/png"), "png");
        assert_eq!(ext_for_mime("image/jpeg"), "jpg");
        assert_eq!(ext_for_mime("image/webp"), "webp");
        assert_eq!(ext_for_mime("image/bmp"), "bmp");
        // 未知类型兜底 png（剪贴板截图几乎总是 png）。
        assert_eq!(ext_for_mime("application/octet-stream"), "png");
    }

    #[test]
    fn stage_clipboard_image_writes_temp_and_uploads() {
        use base64::Engine;
        let bytes: &[u8] = b"fake-clipboard-image-bytes";
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let picked = stage_clipboard_image("image/png".into(), b64).unwrap();
        assert!(picked.name.ends_with(".png"));
        assert!(
            picked.temp_path.contains("mistake-agent-"),
            "temp 需带白名单前缀"
        );
        assert_eq!(std::fs::read(&picked.temp_path).unwrap(), bytes);
        assert_eq!(std::fs::read(&picked.asset_path).unwrap(), bytes);
        let _ = std::fs::remove_file(&picked.temp_path);
        let _ = std::fs::remove_file(&picked.asset_path);
    }

    #[test]
    fn stage_clipboard_image_rejects_invalid_base64() {
        assert!(stage_clipboard_image("image/png".into(), "!!!not-base64!!!".into()).is_err());
    }
}
