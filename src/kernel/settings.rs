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
    /// 模型 ID（主模型默认 deepseek-v4-flash；视觉默认 Qwen/Qwen3-VL-32B-Instruct）。
    #[serde(default)]
    pub model: Option<String>,
    /// 主模型默认 responses（ADR-0020）；Ollama 等不兼容端点配 chat_completions。
    #[serde(default)]
    pub transport: Option<Transport>,
}

/// 设置补丁：GUI 经 set_settings 提交，空字符串/None 表示不改。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsPatch {
    pub log_level: Option<Level>,
    pub english_mode: Option<bool>,
    pub main_model: Option<ModelConfigPatch>,
    pub vision_model: Option<ModelConfigPatch>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelConfigPatch {
    pub api_url: Option<String>,
    /// 空字符串 = 保留原 key；非空 = 覆盖。
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub transport: Option<Transport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_log_level")]
    pub log_level: Level,
    #[serde(default)]
    pub english_mode: bool,
    pub main_model: ModelConfig,
    pub vision_model: ModelConfig,
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
                main_model: ModelConfig {
                    api_url: "https://api.deepseek.com".into(),
                    api_key: String::new(),
                    model: Some("deepseek-v4-flash".into()),
                    transport: Some(Transport::Responses),
                },
                vision_model: ModelConfig {
                    api_url: "https://api.siliconflow.cn/v1".into(),
                    api_key: String::new(),
                    model: Some("Qwen/Qwen3-VL-32B-Instruct".into()),
                    transport: None,
                },
            });
        };
        let main_url =
            env::var("DEEPSEEK_API_URL").unwrap_or_else(|_| "https://api.deepseek.com".into());
        let vision_key = env::var("SILICONFLOW_API_KEY").ok();
        let vision_url = env::var("SILICONFLOW_API_URL")
            .unwrap_or_else(|_| "https://api.siliconflow.cn/v1".into());
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
            main_model: ModelConfig {
                api_url: main_url,
                api_key: main_key,
                model: None,
                transport: Some(Transport::Responses),
            },
            vision_model: ModelConfig {
                api_url: vision_url,
                api_key: vision_key.unwrap_or_default(),
                model: env::var("SILICONFLOW_MODEL")
                    .ok()
                    .or_else(|| Some("Qwen/Qwen3-VL-32B-Instruct".into())),
                transport: None,
            },
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
        let main = &mut self.main_model;
        let vision = &mut self.vision_model;
        Self::apply_model_patch(main, patch.main_model.as_ref(), "main_model")?;
        Self::apply_model_patch(vision, patch.vision_model.as_ref(), "vision_model")?;
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

    /// 面向前端的公开视图：**绝不包含 api_key**，只给 key_set 标记。
    pub fn public_view(&self) -> serde_json::Value {
        json!({
            "log_level": self.log_level,
            "english_mode": self.english_mode,
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
