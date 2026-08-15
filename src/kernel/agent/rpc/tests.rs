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
async fn switch_tool_call_not_persisted_and_children_reparented() {
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStorage::new());
    let key = SessionKey::new();
    store
        .create_session(&key, &SessionMeta::new(key))
        .await
        .unwrap();
    let user = Message::user("帮我批改数学作业");
    store.append_message(&key, &user).await.unwrap();

    let mut switch = Message::tool_call(
        "session::switch",
        json!({"goal": "批改英语作业"}),
        Ok(json!({"switched": true})),
    );
    switch.parent_id = Some(user.id);
    let mut answer = Message::assistant("好的，先切换到英语作业");
    answer.parent_id = Some(switch.id);
    let answer_id = answer.id;

    let last = persist_turn_messages(&store, &key, &[switch, answer], None)
        .await
        .unwrap();
    assert_eq!(last, Some(answer_id));

    let path = store.read_path(&key).await.unwrap();
    assert_eq!(path.len(), 2, "切换控制消息不应落盘");
    assert!(!path[1].is_switch_tool_call());
    assert_eq!(
        path[1].parent_id,
        Some(user.id),
        "子消息父链应重接到切换前最后一条"
    );
}
