//! practice 插件：分层变式练习（场景二入口：薄弱点定位 + 分层变式练习）。
//!
//! 插件信息：namespace = practice，requires = [Storage, Model, Memory, Compute]
//! tools = [generate（变式练习）, gaps（薄弱点定位）, check（练习答案批改）]
//! 实现拆分（Linux 内核风格）：`templates.rs` 模板库（题目/答案/图纸同源）；`gaps.rs` 薄弱点聚合；
//! `check.rs` 答案批改；`history.rs` 练习历史与防重复；`exam_pool.rs` 高考真题池；`generate.rs` LLM 自由出题

use serde_json::{Value, json};

use crate::kernel::agent::dispatch::ToolCallContext;
use crate::kernel::context::PluginContext;
use crate::kernel::contract::{CallerPolicy, Info, PluginError, ToolDef, ToolError};
use crate::kernel::plugin::services::{
    AbortSignal, ComputeHandle, MemoryHandle, ModelHandle, ServiceId, StorageHandle,
};
use crate::kernel::registry::{PluginDescriptor, UserPlugin};

mod check;
mod exam_pool;
mod generate;
mod gaps;
mod geometry_check;
mod history;
mod templates;

use check::{CheckParams, check_handler};
use exam_pool::{draw_from_pool, read_pool_json};
use generate::generate_with_check;
use gaps::{GapsParams, gaps_handler};
use history::recent_mastered;
use templates::GenerateParams;
pub use templates::{Difficulty, PracticeItem, SUPPORTED_POINTS, build_item};

pub struct PracticePlugin;

impl UserPlugin for PracticePlugin {
    fn info() -> Info {
        Info {
            namespace: "practice".into(),
            requires: vec![
                ServiceId::Storage,
                ServiceId::Model,
                ServiceId::Memory,
                ServiceId::Compute,
            ],
            tools: vec![ToolDef {
                name: "generate".into(),
                user_visible: true,
                title: Some("生成变式练习".into()),
                group: Some("学习".into()),
                description:
                    "按知识点生成分层变式练习（基础/同类变式/综合拔高），几何题附带图纸规格；难度传 exam 时从高考真题池抽取（随包题库、标注来源）。用法：practice::generate <知识点> [难度]".into(),
                params: schemars::schema_for!(GenerateParams),
                policy: CallerPolicy::UserAndModel,
                timeout: None,
                icon: Some("mdi:creation".into()),
            },
            ToolDef {
                name: "gaps".into(),
                user_visible: true,
                title: Some("薄弱点定位".into()),
                group: Some("学习".into()),
                description: "基于错题本聚合近 N 天薄弱知识点（按错误次数排序，含建议起点难度），用于定位知识漏洞后出题。用法：practice::gaps [学科] [天数] [数量]".into(),
                params: schemars::schema_for!(GapsParams),
                policy: CallerPolicy::UserAndModel,
                timeout: None,
                icon: Some("mdi:target".into()),
            },
            ToolDef {
                name: "check".into(),
                user_visible: true,
                title: Some("练习答案批改".into()),
                group: Some("学习".into()),
                description: "批改一道练习作答：参考答案可对拍时直接判分，否则由模型判分；答错自动回写错题本。用法：practice::check <题目> <学生答案> [参考答案] [学科] [知识点]".into(),
                params: schemars::schema_for!(CheckParams),
                policy: CallerPolicy::UserAndModel,
                timeout: Some(60),
                icon: Some("mdi:check-decagram".into()),
            }],
            ..Default::default()
        }
    }

    fn register(ctx: PluginContext<'_>) -> Result<(), PluginError> {
        let storage = ctx
            .handles
            .storage()
            .cloned()
            .ok_or_else(|| PluginError::Internal("缺少 Storage 句柄".into()))?;
        let model = ctx
            .handles
            .model()
            .cloned()
            .ok_or_else(|| PluginError::Internal("缺少 Model 句柄".into()))?;
        let memory = ctx
            .handles
            .memory()
            .cloned()
            .ok_or_else(|| PluginError::Internal("缺少 Memory 句柄".into()))?;
        let compute = ctx
            .handles
            .compute()
            .cloned()
            .ok_or_else(|| PluginError::Internal("缺少 Compute 句柄".into()))?;

        let model_for_generate = model.clone();
        let memory_for_generate = memory.clone();
        let compute_for_generate = compute.clone();
        let storage_for_generate = storage.clone();
        ctx.registrar.tool(
            "generate",
            std::sync::Arc::new(move |call_ctx: &ToolCallContext, params: Value| {
                let model = model_for_generate.clone();
                let memory = memory_for_generate.clone();
                let compute = compute_for_generate.clone();
                let storage = storage_for_generate.clone();
                let signal = call_ctx.signal.clone();
                let english_mode = call_ctx.english_mode;
                Box::pin(async move {
                    generate_handler(model, memory, compute, storage, signal, english_mode, params)
                        .await
                })
            }),
        )?;

        let storage_for_gaps = storage.clone();
        ctx.registrar.tool(
            "gaps",
            std::sync::Arc::new(move |_call_ctx: &ToolCallContext, params: Value| {
                let storage = storage_for_gaps.clone();
                Box::pin(async move { gaps_handler(storage, params).await })
            }),
        )?;

        let storage_for_check = storage.clone();
        let model_for_check = model.clone();
        let memory_for_check = memory.clone();
        ctx.registrar.tool(
            "check",
            std::sync::Arc::new(move |call_ctx: &ToolCallContext, params: Value| {
                let model = model_for_check.clone();
                let storage = storage_for_check.clone();
                let memory = memory_for_check.clone();
                let english_mode = call_ctx.english_mode;
                Box::pin(async move {
                    check_handler(model, storage, memory, english_mode, params).await
                })
            }),
        )?;

        Ok(())
    }
}

pub fn descriptor() -> PluginDescriptor {
    PluginDescriptor::from_plugin::<PracticePlugin>()
}

async fn generate_handler(
    model: ModelHandle,
    memory: MemoryHandle,
    compute: ComputeHandle,
    storage: StorageHandle,
    signal: AbortSignal,
    english_mode: bool,
    params: Value,
) -> Result<Value, ToolError> {
    let p: GenerateParams =
        serde_json::from_value(params).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let knowledge_point = p.knowledge_point.trim();
    if knowledge_point.is_empty() {
        return Err(ToolError::invalid_params("knowledge_point 不能为空"));
    }
    let difficulty = p.difficulty.unwrap_or_default();
    // 防重复：近期已掌握题目标识（近 30 天答对的），模板/真题池/LLM 生成均避开。
    let mastered = recent_mastered(&memory).await;
    // 真题层：只走池内抽取（真实来源），不走模板与 LLM 生成。
    if difficulty == Difficulty::Exam {
        // 运行时数据文件优先，缺失/损坏回退内置种子（ADR-0042 数据运行时化）。
        let pool_json = read_pool_json(&storage).await;
        return match draw_from_pool(&pool_json, knowledge_point, &mastered) {
            Some(item) => Ok(json!({ "matched": true, "source": "exam_pool", "item": item })),
            None => Ok(json!({
                "matched": false,
                "message": "真题池暂未收录该知识点的未做题目；可改用基础/同类变式/综合拔高难度，或换用支持的知识点。",
            })),
        };
    }
    match build_item(knowledge_point, difficulty) {
        Some(item) => {
            if mastered.contains(&item.template_id) {
                // 模板命中但近期已掌握 → LLM 兜底生成同知识点新题
                // （注入已掌握清单避开；几何图经可解性校验，失败重出）。
                match generate_with_check(
                    &compute,
                    &model,
                    english_mode,
                    knowledge_point,
                    difficulty,
                    &mastered,
                    &signal,
                )
                .await
                {
                    Ok((item, checked)) => Ok(json!({
                        "matched": true,
                        "source": "llm",
                        "geometry_checked": checked,
                        "item": item,
                    })),
                    Err(e) => Ok(json!({
                        "matched": false,
                        "supported": SUPPORTED_POINTS,
                        "message": format!("该知识点近期已练习且掌握，模型出新题失败：{}", e.message),
                    })),
                }
            } else {
                Ok(json!({ "matched": true, "item": item }))
            }
        }
        // P1 智能出题：模板未命中时走 LLM 生成（结构化 schema，见 generate.rs）；
        // 生成失败回退为未命中提示，工具始终可用；几何图走可解性校验。
        None => match generate_with_check(
            &compute,
            &model,
            english_mode,
            knowledge_point,
            difficulty,
            &mastered,
            &signal,
        )
        .await
        {
            Ok((item, checked)) => Ok(json!({
                "matched": true,
                "source": "llm",
                "geometry_checked": checked,
                "item": item,
            })),
            Err(e) => Ok(json!({
                "matched": false,
                "supported": SUPPORTED_POINTS,
                "message": format!("模板未命中且模型生成失败：{}", e.message),
            })),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::audit::{Auditor, MemoryAuditSink};
    use crate::kernel::plugin::services::{
        AbortSignal, ComputeError, ComputeHandle, ModelError, ModelHandle, ModelRequest,
        ModelResponse, ModelService, ModelStream, MemoryHandle,
    };
    use crate::plugin::practice::geometry_check::tests::FakeCompute;
    use crate::kernel::plugin::storage::MemoryStorage;
    use crate::plugin::practice::history::tests::FakeMemory;
    use crate::plugin::practice::history::record_attempt;
    use crate::plugin::practice::templates::*;
    use std::sync::{Arc, Mutex};

    struct FakeModel {
        reply: Mutex<String>,
    }

    #[async_trait::async_trait]
    impl ModelService for FakeModel {
        async fn stream(
            &self,
            _request: &ModelRequest,
            _signal: &AbortSignal,
        ) -> Result<ModelStream, ModelError> {
            unreachable!("FakeModel 只服务于 complete")
        }

        async fn complete(
            &self,
            _request: &ModelRequest,
            _signal: &AbortSignal,
        ) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse {
                text: self.reply.lock().expect("poisoned").clone(),
                tool_calls: Vec::new(),
                usage: None,
            })
        }
    }

    fn fake_handle(
        reply: &str,
    ) -> (
        ModelHandle,
        MemoryHandle,
        ComputeHandle,
        crate::kernel::plugin::services::StorageHandle,
    ) {
        let model: Arc<dyn ModelService> = Arc::new(FakeModel {
            reply: Mutex::new(reply.into()),
        });
        let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
        let store: Arc<MemoryStorage> = Arc::new(MemoryStorage::new());
        (
            ModelHandle::new(model, std::time::Duration::from_secs(5), auditor),
            MemoryHandle::new(Arc::new(FakeMemory::default())),
            ComputeHandle::new(Arc::new(FakeCompute::default())),
            crate::kernel::plugin::services::StorageHandle::new(store.clone())
                .with_io(store.clone(), store.clone()),
        )
    }

    #[test]
    fn schema_parses_all_difficulties() {
        for d in ["basic", "variant", "advanced"] {
            let p: GenerateParams = serde_json::from_value(json!({
                "knowledge_point": "绝对值",
                "difficulty": d,
            }))
            .unwrap();
            assert_eq!(
                serde_json::to_value(p.difficulty.unwrap()).unwrap(),
                json!(d)
            );
        }
    }

    #[tokio::test]
    async fn generate_returns_matched_item() {
        let (model, memory, compute, storage) =
            fake_handle(r#"{"question_text":"不应走到模型生成","answer_spec":""}"#);
        let out = generate_handler(
            model,
            memory,
            compute,
            storage,
            AbortSignal::new(),
            false,
            json!({
                "knowledge_point": "三角形全等判定",
                "difficulty": "basic",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["matched"], true);
        assert_eq!(out["item"]["template_id"], "triangle_sss");
        let spec = out["item"]["diagram_spec"].clone();
        assert!(spec["points"].is_object());
        assert!(spec["objects"].as_array().unwrap().len() >= 6);
    }

    #[tokio::test]
    async fn generate_llm_fallback_returns_item() {
        let (model, memory, compute, storage) = fake_handle(
            r#"{"knowledge_point":"数列","question_text":"求等差数列 $1,4,7,\\ldots$ 的第 10 项。","answer_spec":"$a_{10}=28$","diagram_spec":null}"#,
        );
        let out = generate_handler(
            model,
            memory,
            compute,
            storage,
            AbortSignal::new(),
            false,
            json!({
                "knowledge_point": "数列",
                "difficulty": "variant",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["matched"], true);
        assert_eq!(out["source"], "llm");
        assert_eq!(out["item"]["template_id"], "llm_freeform");
        assert_eq!(out["item"]["difficulty"], "variant");
        assert_eq!(out["item"]["question_text"], "求等差数列 $1,4,7,\\ldots$ 的第 10 项。");
    }

    #[tokio::test]
    async fn generate_llm_unparseable_falls_back_to_miss() {
        let (model, memory, compute, storage) = fake_handle("抱歉，我无法出题。");
        let out = generate_handler(
            model,
            memory,
            compute,
            storage,
            AbortSignal::new(),
            false,
            json!({
                "knowledge_point": "量子力学",
                "difficulty": "variant",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["matched"], false);
        assert_eq!(out["supported"].as_array().unwrap().len(), 15);
        assert!(out["message"].as_str().unwrap().contains("模型生成失败"));
    }

    #[tokio::test]
    async fn generate_exam_pool_draws_item() {
        let (model, memory, compute, storage) = fake_handle("不应走到模型生成");
        let out = generate_handler(
            model,
            memory,
            compute,
            storage,
            AbortSignal::new(),
            false,
            json!({
                "knowledge_point": "集合运算",
                "difficulty": "exam",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["matched"], true);
        assert_eq!(out["source"], "exam_pool");
        assert_eq!(out["item"]["difficulty"], "exam");
        assert!(out["item"]["template_id"].as_str().unwrap().starts_with("exam:"));
        assert!(out["item"]["source"].as_str().unwrap().contains("卷"));
    }

    #[tokio::test]
    async fn generate_exam_pool_miss_returns_message() {
        let (model, memory, compute, storage) = fake_handle("不应走到模型生成");
        let out = generate_handler(
            model,
            memory,
            compute,
            storage,
            AbortSignal::new(),
            false,
            json!({
                "knowledge_point": "量子力学",
                "difficulty": "exam",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["matched"], false);
        assert!(out["message"].as_str().unwrap().contains("真题池"));
    }

    #[tokio::test]
    async fn generate_skips_mastered_template_via_llm() {
        // 预置：triangle_sss 近期已答对（已掌握）。
        let (model, memory, compute, storage) = fake_handle(
            r#"{"knowledge_point":"三角形全等判定","question_text":"如图，在 △ABC 与 △DEF 中，AB=DE，∠A=∠D，AC=DF，请判断全等并说明依据。","answer_spec":"全等（SAS）","diagram_spec":null}"#,
        );
        record_attempt(
            &memory,
            "triangle_sss",
            "三角形全等判定",
            Difficulty::Basic,
            true,
        )
        .await;
        let out = generate_handler(
            model,
            memory,
            compute,
            storage,
            AbortSignal::new(),
            false,
            json!({
                "knowledge_point": "三角形全等判定",
                "difficulty": "basic",
            }),
        )
        .await
        .unwrap();
        // 模板命中但已掌握 → 改走 LLM 出新题。
        assert_eq!(out["matched"], true);
        assert_eq!(out["source"], "llm");
        assert_eq!(out["item"]["template_id"], "llm_freeform");
    }

    #[tokio::test]
    async fn generate_llm_geometry_checked_ok() {
        // LLM 返回带图形规格的题，compute 校验通过 → geometry_checked=true。
        let (model, memory, compute, storage) = fake_handle(
            r#"{"knowledge_point":"圆与切线","question_text":"如图，PA 是圆 O 的切线。","answer_spec":"PA⊥OA","diagram_spec":{"points":{"O":[0,0],"A":[3,0],"P":[3,4]},"objects":[{"type":"circle","center":"O","radius":3},{"type":"segment","ends":["O","A"]},{"type":"segment","ends":["A","P"]}],"labels":["O","A","P"]}}"#,
        );
        let out = generate_handler(
            model,
            memory,
            compute,
            storage,
            AbortSignal::new(),
            false,
            json!({
                "knowledge_point": "圆与切线",
                "difficulty": "variant",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["matched"], true);
        assert_eq!(out["source"], "llm");
        assert_eq!(out["geometry_checked"], true);
    }

    #[tokio::test]
    async fn generate_llm_geometry_retry_then_ok() {
        // 第一次校验失败（三点共线），第二次通过 → 重出成功。
        let (model, memory, _compute, storage) = fake_handle(
            r#"{"knowledge_point":"圆与切线","question_text":"如图，PA 是圆 O 的切线。","answer_spec":"PA⊥OA","diagram_spec":{"points":{"O":[0,0],"A":[3,0],"P":[3,4]},"objects":[{"type":"circle","center":"O","radius":3}],"labels":["O","A","P"]}}"#,
        );
        let compute = ComputeHandle::new(Arc::new(FakeCompute {
            results: Mutex::new(vec![
                Ok(Some("多边形三点共线: A,B,C".into())),
                Ok(None),
            ]),
        }));
        let out = generate_handler(
            model,
            memory,
            compute,
            storage,
            AbortSignal::new(),
            false,
            json!({
                "knowledge_point": "圆与切线",
                "difficulty": "variant",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["matched"], true);
        assert_eq!(out["source"], "llm");
        assert_eq!(out["geometry_checked"], true);
    }

    #[tokio::test]
    async fn generate_llm_geometry_exhausted_returns_message() {
        // 连续 3 次校验失败 → 停止（复用工具护栏语义），回告模型。
        let (model, memory, _compute, storage) = fake_handle(
            r#"{"knowledge_point":"圆与切线","question_text":"如图，PA 是圆 O 的切线。","answer_spec":"PA⊥OA","diagram_spec":{"points":{"O":[0,0],"A":[3,0],"P":[3,4]},"objects":[{"type":"circle","center":"O","radius":3}],"labels":["O","A","P"]}}"#,
        );
        let compute = ComputeHandle::new(Arc::new(FakeCompute {
            results: Mutex::new(vec![
                Ok(Some("三点共线".into())),
                Ok(Some("三角不等式不成立".into())),
                Ok(Some("半径非法".into())),
            ]),
        }));
        let out = generate_handler(
            model,
            memory,
            compute,
            storage,
            AbortSignal::new(),
            false,
            json!({
                "knowledge_point": "圆与切线",
                "difficulty": "variant",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["matched"], false);
        assert!(out["message"].as_str().unwrap().contains("连续 3 次"));
    }

    #[tokio::test]
    async fn generate_llm_geometry_backend_down_degrades() {
        // 执行端不可用：降级放行（geometry_checked=false），不阻塞出题。
        let (model, memory, _compute, storage) = fake_handle(
            r#"{"knowledge_point":"圆与切线","question_text":"如图，PA 是圆 O 的切线。","answer_spec":"PA⊥OA","diagram_spec":{"points":{"O":[0,0],"A":[3,0],"P":[3,4]},"objects":[{"type":"circle","center":"O","radius":3}],"labels":["O","A","P"]}}"#,
        );
        let compute = ComputeHandle::new(Arc::new(FakeCompute {
            results: Mutex::new(vec![Err(ComputeError::BackendUnavailable)]),
        }));
        let out = generate_handler(
            model,
            memory,
            compute,
            storage,
            AbortSignal::new(),
            false,
            json!({
                "knowledge_point": "圆与切线",
                "difficulty": "variant",
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["matched"], true);
        assert_eq!(out["source"], "llm");
        assert_eq!(out["geometry_checked"], false);
    }

    #[test]
    fn three_difficulties_differ_per_template() {
        let qs: Vec<_> = [Difficulty::Basic, Difficulty::Variant, Difficulty::Advanced]
            .iter()
            .map(|d| absolute_value(*d).question_text)
            .collect();
        assert!(qs.windows(2).all(|w| w[0] != w[1]));

        let geo: Vec<_> = [Difficulty::Basic, Difficulty::Variant, Difficulty::Advanced]
            .iter()
            .map(|d| triangle_congruence(*d).template_id.clone())
            .collect();
        assert_eq!(geo.len(), 3);
        assert!(geo[0] != geo[1] && geo[1] != geo[2]);
    }

    #[test]
    fn geometry_diagram_has_labels() {
        let item = triangle_congruence(Difficulty::Advanced);
        let spec = item.diagram_spec.unwrap();
        assert_eq!(spec["labels"].as_array().unwrap().len(), 4);
    }
}
