use super::*;
use crate::kernel::agent::rpc::protocol::custom_params;
use crate::kernel::agent::session::SessionMeta;
use crate::kernel::audit::{Auditor, MemoryAuditSink};
use crate::kernel::plugin::services::{
    AbortSignal, ModelChunk, ModelError, ModelResponse, ModelStream,
};
use crate::kernel::plugin::storage::MemoryStorage;

#[test]
fn rpc_wire_parses_generic_and_custom_methods() {
    let generic: RpcRequest = serde_json::from_str(r#"{"id":1,"method":"get_state"}"#).unwrap();
    assert!(matches!(
        generic.method,
        WireMethod::Generic(Method::GetState)
    ));

    let custom: RpcRequest = serde_json::from_str(r#"{"id":2,"method":"check_balance"}"#).unwrap();
    let WireMethod::Custom(custom) = custom.method else {
        panic!("未知方法应落入 custom 兜底");
    };
    assert_eq!(custom.method, "check_balance");

    let compute: RpcRequest = serde_json::from_str(
        r#"{"id":3,"method":"compute_result","compute_id":9,"stdout":"ok","stderr":"","duration_ms":1}"#,
    )
    .unwrap();
    let WireMethod::Custom(compute) = compute.method else {
        panic!("compute_result 应落入 custom 兜底");
    };
    assert_eq!(compute.extra["compute_id"], 9);
    let merged = custom_params(&compute);
    assert_eq!(merged["compute_id"], 9);
    assert_eq!(merged["stdout"], "ok");

    let create: RpcRequest =
        serde_json::from_str(r#"{"id":4,"method":"create_session","carry_summary":true}"#).unwrap();
    let WireMethod::Generic(Method::CreateSession {
        carry_summary,
        goal,
    }) = create.method
    else {
        panic!("create_session 应解析为通用方法");
    };
    assert!(carry_summary);
    assert!(goal.is_none());

    // 会话列表三个新方法（ADR-0044 收尾）走同一 wire 通道。
    let key = SessionKey::new();
    let open: RpcRequest = serde_json::from_str(&format!(
        r#"{{"id":5,"method":"open_session","key":"{key}"}}"#
    ))
    .unwrap();
    let WireMethod::Generic(Method::OpenSession { key: parsed }) = open.method else {
        panic!("open_session 应解析为通用方法");
    };
    assert_eq!(parsed, key);

    let rename: RpcRequest = serde_json::from_str(&format!(
        r#"{{"id":6,"method":"rename_session","key":"{key}","title":"线性代数"}}"#
    ))
    .unwrap();
    let WireMethod::Generic(Method::RenameSession { title, .. }) = rename.method else {
        panic!("rename_session 应解析为通用方法");
    };
    assert_eq!(title, "线性代数");

    let delete: RpcRequest = serde_json::from_str(&format!(
        r#"{{"id":7,"method":"delete_session","key":"{key}"}}"#
    ))
    .unwrap();
    assert!(matches!(
        delete.method,
        WireMethod::Generic(Method::DeleteSession { .. })
    ));
}

struct StubBuilderModel;

#[async_trait::async_trait]
impl ModelService for StubBuilderModel {
    async fn stream(
        &self,
        _request: &ModelRequest,
        _signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError> {
        Ok(Box::new(futures_util::stream::empty::<
            Result<ModelChunk, ModelError>,
        >()))
    }

    async fn complete(
        &self,
        _request: &ModelRequest,
        _signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        Err(ModelError::Transport("stub".into()))
    }
}

struct PingExtension;

#[async_trait::async_trait]
impl RpcExtension for PingExtension {
    async fn handle(&self, method: &str, _params: Value) -> Result<Option<Value>, RpcError> {
        if method == "ping" {
            Ok(Some(json!({"pong": true})))
        } else {
            Ok(None)
        }
    }
}

#[tokio::test]
async fn kernel_builder_assembles_and_routes_custom_method() {
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStorage::new());
    let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
    let kernel = KernelBuilder::new()
        .session_store(store)
        .main_model(Arc::new(StubBuilderModel))
        .auditor(auditor)
        .extension(Arc::new(PingExtension))
        .build()
        .await
        .unwrap();
    let frame = kernel
        .handle(RpcRequest::custom(1, "ping", json!({})))
        .await
        .unwrap()
        .expect("应有响应帧");
    assert!(
        serde_json::to_string(&frame)
            .unwrap()
            .contains("\"pong\":true")
    );
}

#[tokio::test]
async fn create_session_rpc_archives_old_and_opens_new() {
    let storage = MemoryStorage::new();
    let store: Arc<dyn SessionStore> = Arc::new(storage.clone());
    let key = SessionKey::new();
    store
        .create_session(&key, &SessionMeta::new(key))
        .await
        .unwrap();
    store
        .append_message(&key, &Message::user("帮我看看这道题"))
        .await
        .unwrap();
    store
        .append_message(&key, &Message::assistant("这道题先看定义域"))
        .await
        .unwrap();

    let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
    let kernel = KernelBuilder::new()
        .session_store(store.clone())
        .main_model(Arc::new(StubBuilderModel))
        .auditor(auditor)
        .build()
        .await
        .unwrap();

    // 短会话（<8 条）走 stub 摘要，不触发语言模型调用。
    let frame = kernel
        .handle(RpcRequest {
            id: 1,
            method: WireMethod::Generic(Method::CreateSession {
                carry_summary: true,
                goal: None,
            }),
        })
        .await
        .unwrap()
        .expect("应有响应帧");
    let RpcFrame::Response { result, error, .. } = frame else {
        panic!("应为响应帧");
    };
    assert!(error.is_none(), "不应报错：{error:?}");
    let result = result.unwrap();
    assert_eq!(result["summary_attached"], true);
    assert_eq!(result["archived_session_key"], json!(key));
    let new_key: SessionKey = serde_json::from_value(result["session_key"].clone()).unwrap();
    assert_ne!(new_key, key);

    let metas = store.list_sessions().await.unwrap();
    assert_eq!(metas.len(), 2);
    assert_eq!(
        metas
            .iter()
            .filter(|m| m.status == SessionStatus::Active)
            .count(),
        1,
        "归档后应只剩一个活动会话"
    );
    assert_eq!(
        metas.iter().find(|m| m.key == key).unwrap().status,
        SessionStatus::Archived
    );
    let path = store.read_path(&new_key).await.unwrap();
    assert_eq!(path.len(), 1);
    assert!(matches!(
        path[0].kind,
        crate::kernel::message::MessageKind::System { ref text, .. }
            if text.contains("上一会话梗概")
    ));
}

#[tokio::test]
async fn removed_switch_tool_is_unknown() {
    // session::switch 已下线（ADR-0044）：强制调用应报未知工具，而非静默切换会话。
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStorage::new());
    let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
    let kernel = KernelBuilder::new()
        .session_store(store)
        .main_model(Arc::new(StubBuilderModel))
        .auditor(auditor)
        .build()
        .await
        .unwrap();
    let err = kernel
        .handle(RpcRequest {
            id: 1,
            method: WireMethod::Generic(Method::SendUserMessage {
                text: "换个话题".into(),
                display_text: None,
                force_tool: Some(ForcedToolRequest {
                    entry: "session::switch".into(),
                    hint: None,
                    display: None,
                }),
                file: Vec::new(),
                asset: Vec::new(),
            }),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, "unknown_tool");
}

/// 装配内核 + 两条会话：返回的 key 为 Active，other 为 Archived。
async fn kernel_with_two_sessions() -> (Arc<Kernel>, Arc<dyn SessionStore>, SessionKey, SessionKey)
{
    let storage = MemoryStorage::new();
    let store: Arc<dyn SessionStore> = Arc::new(storage.clone());
    let key = SessionKey::new();
    store
        .create_session(&key, &SessionMeta::new(key))
        .await
        .unwrap();
    store
        .append_message(&key, &Message::user("当前会话"))
        .await
        .unwrap();

    let other = SessionKey::new();
    let mut other_meta = SessionMeta::new(other);
    other_meta.status = SessionStatus::Archived;
    store.create_session(&other, &other_meta).await.unwrap();
    store
        .append_message(&other, &Message::user("另一条会话"))
        .await
        .unwrap();

    let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
    let kernel = KernelBuilder::new()
        .session_store(store.clone())
        .main_model(Arc::new(StubBuilderModel))
        .auditor(auditor)
        .build()
        .await
        .unwrap();
    (kernel, store, key, other)
}

async fn call(kernel: &Kernel, method: Method) -> Value {
    let frame = kernel
        .handle(RpcRequest {
            id: 1,
            method: WireMethod::Generic(method),
        })
        .await
        .unwrap()
        .expect("应有响应帧");
    let RpcFrame::Response { result, error, .. } = frame else {
        panic!("应为响应帧");
    };
    assert!(error.is_none(), "不应报错：{error:?}");
    result.unwrap()
}

#[tokio::test]
async fn open_session_archives_previous_active() {
    let (kernel, store, key, other) = kernel_with_two_sessions().await;

    let result = call(&kernel, Method::OpenSession { key: other }).await;
    assert_eq!(result["session_key"], json!(other));

    let metas = store.list_sessions().await.unwrap();
    assert_eq!(
        metas
            .iter()
            .filter(|m| m.status == SessionStatus::Active)
            .count(),
        1,
        "切换后仍只有一条 Active"
    );
    assert_eq!(
        metas.iter().find(|m| m.key == other).unwrap().status,
        SessionStatus::Active
    );
    assert_eq!(
        metas.iter().find(|m| m.key == key).unwrap().status,
        SessionStatus::Archived
    );

    // 不存在的会话：报错而非静默成功。
    let err = kernel
        .handle(RpcRequest {
            id: 2,
            method: WireMethod::Generic(Method::OpenSession {
                key: SessionKey::new(),
            }),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, "scheduler_error");
}

#[tokio::test]
async fn delete_active_session_leaves_exactly_one_active() {
    let (kernel, store, key, other) = kernel_with_two_sessions().await;

    let result = call(&kernel, Method::DeleteSession { key }).await;
    let replacement: SessionKey =
        serde_json::from_value(result["replacement_session_key"].clone()).unwrap();
    assert_ne!(replacement, key, "删掉活动会话应补建一条新的");

    let metas = store.list_sessions().await.unwrap();
    assert!(metas.iter().all(|m| m.key != key), "被删会话应消失");
    assert_eq!(metas.len(), 2, "另一条会话保留 + 补建的空会话");
    assert_eq!(
        metas
            .iter()
            .filter(|m| m.status == SessionStatus::Active)
            .map(|m| m.key)
            .collect::<Vec<_>>(),
        vec![replacement],
        "补建后仍只有一条 Active"
    );
    assert!(
        store.read_all(&replacement).await.unwrap().is_empty(),
        "补建的是空会话"
    );

    // 删归档会话：不补建（活动会话不受影响）。
    let result = call(&kernel, Method::DeleteSession { key: other }).await;
    assert_eq!(result["replacement_session_key"], Value::Null);
    let metas = store.list_sessions().await.unwrap();
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0].key, replacement);
}

#[tokio::test]
async fn rename_session_persists_and_empty_title_clears() {
    let (kernel, store, key, _) = kernel_with_two_sessions().await;

    let result = call(
        &kernel,
        Method::RenameSession {
            key,
            title: "  线性代数错题  ".into(),
        },
    )
    .await;
    assert_eq!(result["title"], json!("线性代数错题"));
    assert_eq!(
        store
            .get_session(&key)
            .await
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("线性代数错题")
    );

    // 空串 = 清空（下个回合末可由模型重新生成）。
    let result = call(
        &kernel,
        Method::RenameSession {
            key,
            title: "   ".into(),
        },
    )
    .await;
    assert_eq!(result["title"], Value::Null);
    assert!(
        store
            .get_session(&key)
            .await
            .unwrap()
            .unwrap()
            .title
            .is_none()
    );
}
