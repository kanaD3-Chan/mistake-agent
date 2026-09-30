//! settings（ADR-0015）：用户独占写、kernel 独占读。M1 支持文件 + 环境变量回退。

use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::kernel::logger::Level;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    #[default]
    Responses,
    ChatCompletions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub api_url: String,
    pub api_key: String,
    /// 模型 ID（默认 deepseek-flash）。
    #[serde(default)]
    pub model: Option<String>,
    /// 默认 responses（ADR-0020）；Ollama 等不兼容端点配 chat_completions。
    #[serde(default)]
    pub transport: Option<Transport>,
}

/// 设置补丁：GUI 经 set_settings 提交，空字符串/None 表示不改。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsPatch {
    pub log_level: Option<Level>,
    pub english_mode: Option<bool>,
    /// 昵称：`Some("")` **是清空**（回到默认称呼），与 api_key 的「空串=保留」语义不同。
    pub nickname: Option<String>,
    pub main_model: Option<ModelConfigPatch>,
    pub vision_model: Option<ModelConfigPatch>,
    pub account: Option<AccountPatch>,
}

/// 账号段里允许前端经 `set_settings` 改的字段：**只有服务端地址**。
/// `token`/`email`/`role`/`sync_enabled` 一律由登录/登出/状态刷新三条 RPC 路径按服务端的
/// 回执写入，与 `api_key` 同一纪律——不给前端注入令牌的入口（ADR-0048 决策 2）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountPatch {
    pub server_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelConfigPatch {
    pub api_url: Option<String>,
    /// 空字符串 = 保留原 key；非空 = 覆盖。
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub transport: Option<Transport>,
}

/// 平台服务账号（ADR-0048）：`token` 非空即"已登录"，模型链路改走平台中转。
/// 与 `main_model` 分开存——令牌不是 API Key，混进 `api_key` 会让「切换回自备 Key」变成不可逆动作。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    pub server_url: String,
    /// 平台令牌明文（`mka_` + 64 位十六进制）。空串 = 本地模式。
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub email: String,
    /// 服务端返回的角色，客户端只读展示（user / teacher / admin）。
    #[serde(default)]
    pub role: String,
    /// 服务端开关注解：同步引擎属 S6，本轮只落库、不读。
    #[serde(default)]
    pub sync_enabled: bool,
}

impl AccountConfig {
    pub fn logged_in(&self) -> bool {
        !self.token.trim().is_empty()
    }
}

/// 默认平台服务地址（用户指定）。服务端部署（S8）后仍可在此改。
pub const DEFAULT_SERVER_URL: &str = "http://8.131.146.250:8080";

fn default_account_config() -> AccountConfig {
    AccountConfig {
        server_url: DEFAULT_SERVER_URL.to_string(),
        token: String::new(),
        email: String::new(),
        role: String::new(),
        sync_enabled: false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_log_level")]
    pub log_level: Level,
    #[serde(default)]
    pub english_mode: bool,
    /// 用户昵称：只用于侧栏左下角的称呼显示，空串 = 用前端默认称呼。
    #[serde(default)]
    pub nickname: String,
    pub main_model: ModelConfig,
    /// 已退役（ADR-0045）：仅为兼容旧 settings.json 保留，运行时不再读取。
    #[serde(default = "default_vision_config")]
    pub vision_model: ModelConfig,
    /// 平台服务账号（ADR-0048）。旧 settings.json 无该段时按默认值（未登录、默认地址）解析。
    #[serde(default = "default_account_config")]
    pub account: AccountConfig,
}

/// 昵称长度上限（字符数）：侧栏一行放得下，也挡住把整段文字当名字存进来。
const MAX_NICKNAME_CHARS: usize = 24;

/// 归一化平台服务地址：必须是 http(s)、必须有主机名，尾部斜杠去掉。
/// 客户端两处拼 URL 的写法不同（`responses_endpoint()` 会剥尾部 `/v1`，chat 适配器按原样拼），
/// 统一在这里收口，免得"带不带尾斜杠"变成两种行为。
pub fn normalize_server_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim().trim_end_matches('/');
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err("平台服务地址必须是 http(s) 地址".into());
    }
    let host = trimmed.split_once("://").map(|(_, h)| h).unwrap_or("");
    if host.is_empty() {
        return Err("平台服务地址缺少主机名".into());
    }
    Ok(trimmed.to_string())
}

fn default_vision_config() -> ModelConfig {
    ModelConfig {
        api_url: String::new(),
        api_key: String::new(),
        model: None,
        transport: None,
    }
}

fn default_log_level() -> Level {
    Level::Info
}

impl Settings {
    /// 数据根目录（ADR-0011）。
    pub fn data_root() -> PathBuf {
        let home = env::var("USERPROFILE")
            .or_else(|_| env::var("HOME"))
            .unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join("Documents").join(".mistake-agent")
    }

    pub fn load() -> Result<Self, String> {
        let root = Self::data_root();
        let path = root.join("settings.json");
        if let Ok(text) = std::fs::read_to_string(&path) {
            return serde_json::from_str(&text).map_err(|e| format!("settings.json 解析失败：{e}"));
        }
        // 环境变量回退（开发/集成测试用）；两者都没有时返回空默认配置，
        // 让应用可启动、由 OOBE 引导填写（不阻塞首次使用）。
        let Ok(main_key) = env::var("DEEPSEEK_API_KEY") else {
            return Ok(Self {
                log_level: default_log_level(),
                english_mode: false,
                nickname: String::new(),
                main_model: ModelConfig {
                    api_url: "https://api.deepseek.com".into(),
                    api_key: String::new(),
                    model: Some("deepseek-flash".into()),
                    transport: Some(Transport::Responses),
                },
                vision_model: default_vision_config(),
                account: default_account_config(),
            });
        };
        let main_url =
            env::var("DEEPSEEK_API_URL").unwrap_or_else(|_| "https://api.deepseek.com".into());
        let log_level = match env::var("MISTAKE_AGENT_LOG_LEVEL").as_deref() {
            Ok("debug") => Level::Debug,
            Ok("warn") => Level::Warn,
            Ok("error") => Level::Error,
            Ok("critical") => Level::Critical,
            _ => Level::Info,
        };
        Ok(Self {
            log_level,
            english_mode: false,
            nickname: String::new(),
            main_model: ModelConfig {
                api_url: main_url,
                api_key: main_key,
                model: None,
                transport: Some(Transport::Responses),
            },
            vision_model: default_vision_config(),
            account: default_account_config(),
        })
    }

    /// 应用设置补丁并校验（api_url 必须 http(s)，模型名非空）。
    pub fn apply_patch(&mut self, patch: &SettingsPatch) -> Result<(), String> {
        if let Some(level) = patch.log_level {
            self.log_level = level;
        }
        if let Some(english_mode) = patch.english_mode {
            self.english_mode = english_mode;
        }
        if let Some(nickname) = &patch.nickname {
            let nickname = nickname.trim();
            let len = nickname.chars().count();
            if len > MAX_NICKNAME_CHARS {
                return Err(format!("nickname 不能超过 {MAX_NICKNAME_CHARS} 个字符"));
            }
            self.nickname = nickname.to_string();
        }
        let main = &mut self.main_model;
        let vision = &mut self.vision_model;
        Self::apply_model_patch(main, patch.main_model.as_ref(), "main_model")?;
        Self::apply_model_patch(vision, patch.vision_model.as_ref(), "vision_model")?;
        if let Some(account) = &patch.account
            && let Some(url) = &account.server_url
        {
            self.account.server_url = normalize_server_url(url)?;
        }
        // AccountPatch 里**没有** token/email/role/sync_enabled 字段：
        // 类型层面就堵死了经 set_settings 注入令牌或伪造身份（ADR-0048 决策 2）。
        Ok(())
    }

    fn apply_model_patch(
        cfg: &mut ModelConfig,
        patch: Option<&ModelConfigPatch>,
        name: &str,
    ) -> Result<(), String> {
        let Some(patch) = patch else {
            return Ok(());
        };
        if let Some(url) = &patch.api_url {
            let url = url.trim().to_string();
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err(format!("{name}.api_url 必须是 http(s) 地址"));
            }
            cfg.api_url = url;
        }
        if let Some(key) = &patch.api_key
            && !key.trim().is_empty()
        {
            cfg.api_key = key.trim().to_string();
        }
        if let Some(model) = &patch.model {
            let model = model.trim().to_string();
            if model.is_empty() {
                return Err(format!("{name}.model 不能为空"));
            }
            cfg.model = Some(model);
        }
        if let Some(transport) = patch.transport {
            cfg.transport = Some(transport);
        }
        Ok(())
    }

    /// 保存到 settings.json（原子写；用户经 GUI 独占写，ADR-0015）。
    pub fn save(&self) -> Result<(), String> {
        let path = Self::data_root().join("settings.json");
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 面向前端的公开视图：**绝不包含 api_key，也绝不包含 account.token**。
    /// 账号段只给 `logged_in` 布尔，与 `key_set` 同一纪律。
    pub fn public_view(&self) -> serde_json::Value {
        json!({
            "log_level": self.log_level,
            "english_mode": self.english_mode,
            "nickname": self.nickname,
            "account": {
                "logged_in": self.account.logged_in(),
                "server_url": self.account.server_url,
                "email": self.account.email,
                "role": self.account.role,
                "sync_enabled": self.account.sync_enabled,
            },
            "main_model": {
                "api_url": self.main_model.api_url,
                "model": self.main_model.model,
                "transport": self.main_model.transport,
                "key_set": !self.main_model.api_key.is_empty(),
            },
            "vision_model": {
                "api_url": self.vision_model.api_url,
                "model": self.vision_model.model,
                "transport": self.vision_model.transport,
                "key_set": !self.vision_model.api_key.is_empty(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        Settings {
            log_level: Level::Info,
            english_mode: false,
            nickname: "小明".into(),
            main_model: ModelConfig {
                api_url: "https://api.deepseek.com".into(),
                api_key: "sk-secret-key".into(),
                model: Some("deepseek-v4-flash".into()),
                transport: Some(Transport::Responses),
            },
            vision_model: ModelConfig {
                api_url: "https://api.siliconflow.cn/v1".into(),
                api_key: "sk-vision-key".into(),
                model: Some("Qwen/Qwen3-VL-32B-Instruct".into()),
                transport: None,
            },
            account: AccountConfig {
                server_url: DEFAULT_SERVER_URL.into(),
                token: "mka_secret_token".into(),
                email: "demo@example.test".into(),
                role: "user".into(),
                sync_enabled: false,
            },
        }
    }

    #[test]
    fn public_view_never_leaks_api_key() {
        let view = sample().public_view();
        assert!(view.get("api_key").is_none());
        assert_eq!(view["english_mode"], false);
        assert!(view["main_model"].get("api_key").is_none());
        assert!(view["vision_model"].get("api_key").is_none());
        assert_eq!(view["main_model"]["key_set"], true);
        assert_eq!(view["vision_model"]["key_set"], true);
    }

    #[test]
    fn public_view_never_leaks_account_token() {
        let view = sample().public_view();
        assert!(view["account"].get("token").is_none());
        assert_eq!(view["account"]["logged_in"], true);
        assert_eq!(view["account"]["email"], "demo@example.test");
        assert_eq!(view["account"]["server_url"], DEFAULT_SERVER_URL);
        // 整串视图里也不该出现令牌明文（防哪天有人手滑改成 json! 里直接塞 config）。
        assert!(!view.to_string().contains("mka_secret_token"));
    }

    /// 旧 settings.json（S5 之前写的）没有 account 段，必须仍能解析，
    /// 且落回"未登录 + 默认地址"——否则升级即崩，用户连 OOBE 都进不去。
    #[test]
    fn legacy_settings_without_account_section_still_parses() {
        let legacy = r#"{
            "log_level": "info",
            "english_mode": false,
            "nickname": "小明",
            "main_model": {
                "api_url": "https://api.deepseek.com",
                "api_key": "sk-legacy",
                "model": "deepseek-v4-flash",
                "transport": "responses"
            }
        }"#;
        let settings: Settings = serde_json::from_str(legacy).expect("旧配置必须可解析");
        assert!(!settings.account.logged_in());
        assert_eq!(settings.account.server_url, DEFAULT_SERVER_URL);
        assert_eq!(settings.account.email, "");
        assert_eq!(settings.public_view()["account"]["logged_in"], false);
    }

    #[test]
    fn normalize_server_url_requires_http_and_strips_trailing_slash() {
        assert_eq!(
            normalize_server_url("  https://api.example.com/  ").unwrap(),
            "https://api.example.com"
        );
        assert_eq!(
            normalize_server_url("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        // 多层尾斜杠一次去干净，不能只去掉一个。
        assert_eq!(
            normalize_server_url("http://127.0.0.1:8080///").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert!(normalize_server_url("ftp://bad").is_err());
        assert!(normalize_server_url("api.example.com").is_err());
        assert!(normalize_server_url("http://").is_err());
        assert!(normalize_server_url("").is_err());
    }

    #[test]
    fn account_patch_changes_url_but_cannot_touch_token() {
        let mut settings = sample();
        settings
            .apply_patch(&SettingsPatch {
                account: Some(AccountPatch {
                    server_url: Some("https://platform.example.com/".into()),
                }),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(settings.account.server_url, "https://platform.example.com");
        // 令牌与身份字段原封不动：AccountPatch 里根本没有这些字段可填。
        assert_eq!(settings.account.token, "mka_secret_token");
        assert_eq!(settings.account.email, "demo@example.test");
        assert_eq!(settings.account.role, "user");

        // 非法地址被拒，且不落地（不能改一半）。
        let before = settings.account.server_url.clone();
        assert!(
            settings
                .apply_patch(&SettingsPatch {
                    account: Some(AccountPatch {
                        server_url: Some("ftp://bad".into()),
                    }),
                    ..Default::default()
                })
                .is_err()
        );
        assert_eq!(settings.account.server_url, before);

        // 不传 account 段 = 不动。
        settings
            .apply_patch(&SettingsPatch {
                account: None,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(settings.account.server_url, before);
    }

    #[test]
    fn patch_rejects_invalid_url_and_empty_model() {
        let mut settings = sample();
        let bad_url = SettingsPatch {
            main_model: Some(ModelConfigPatch {
                api_url: Some("ftp://bad".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(settings.apply_patch(&bad_url).is_err());

        let bad_model = SettingsPatch {
            vision_model: Some(ModelConfigPatch {
                model: Some("  ".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(settings.apply_patch(&bad_model).is_err());
    }

    #[test]
    fn patch_applies_english_mode() {
        let mut settings = sample();
        let patch = SettingsPatch {
            english_mode: Some(true),
            ..Default::default()
        };
        settings.apply_patch(&patch).unwrap();
        assert!(settings.english_mode);
        assert_eq!(settings.public_view()["english_mode"], true);
    }

    #[test]
    fn patch_applies_and_clears_nickname() {
        let mut settings = sample();
        settings
            .apply_patch(&SettingsPatch {
                nickname: Some("  张涵  ".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(settings.nickname, "张涵");
        assert_eq!(settings.public_view()["nickname"], "张涵");

        // 空串是**清空**（回到默认称呼），不是 api_key 那种「空串=保留」。
        settings
            .apply_patch(&SettingsPatch {
                nickname: Some(String::new()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(settings.nickname, "");

        // None 才是不动。
        settings.nickname = "小明".into();
        settings.apply_patch(&SettingsPatch::default()).unwrap();
        assert_eq!(settings.nickname, "小明");

        // 上限按**字符**算：正好 24 个汉字（72 字节）放行，25 个拒绝。
        settings
            .apply_patch(&SettingsPatch {
                nickname: Some("字".repeat(MAX_NICKNAME_CHARS)),
                ..Default::default()
            })
            .unwrap();
        assert!(
            settings
                .apply_patch(&SettingsPatch {
                    nickname: Some("字".repeat(MAX_NICKNAME_CHARS + 1)),
                    ..Default::default()
                })
                .is_err()
        );
        assert_eq!(settings.nickname.chars().count(), MAX_NICKNAME_CHARS);
    }

    #[test]
    fn patch_applies_url_model_and_key_keep_empty() {
        let mut settings = sample();
        let patch = SettingsPatch {
            main_model: Some(ModelConfigPatch {
                api_url: Some("https://api.example.com".into()),
                model: Some("deepseek-v4-pro".into()),
                api_key: Some("".into()), // 空串 = 保留原 key
                transport: Some(Transport::ChatCompletions),
            }),
            ..Default::default()
        };
        settings.apply_patch(&patch).unwrap();
        assert_eq!(settings.main_model.api_url, "https://api.example.com");
        assert_eq!(
            settings.main_model.model.as_deref(),
            Some("deepseek-v4-pro")
        );
        assert_eq!(settings.main_model.api_key, "sk-secret-key");
        assert_eq!(
            settings.main_model.transport,
            Some(Transport::ChatCompletions)
        );
    }
}
