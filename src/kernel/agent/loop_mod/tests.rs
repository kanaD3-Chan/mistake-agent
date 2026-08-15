use super::*;
use crate::kernel::agent::session::{Interrupt, InterruptBus, StubSummarizer};
use crate::kernel::audit::MemoryAuditSink;
use crate::kernel::events::MemoryEventSink;
use crate::kernel::plugin::services::{
    AbortSignal, ModelError, ModelResponse, ModelStream, ServiceHandles,
};
use crate::kernel::registry::Registry;

struct ScriptedLoopModel;

#[async_trait::async_trait]
impl ModelService for ScriptedLoopModel {
    async fn stream(
        &self,
        _request: &ModelRequest,
        _signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError> {
        let chunks = vec![
            Ok(ModelChunk::TextDelta("好的，已处理。".into())),
            Ok(ModelChunk::ItemDone {
                kind: ItemKind::Message,
            }),
            Ok(ModelChunk::Done),
        ];
        Ok(Box::new(futures_util::stream::iter(chunks)))
    }

    async fn complete(
        &self,
        _request: &ModelRequest,
        _signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        Err(ModelError::Transport("测试模型不支持 complete".into()))
    }
}

fn setup_loop(
    bus: InterruptBus,
    context_limit: usize,
) -> (Arc<AgentLoop>, Arc<MemoryAuditSink>, Arc<MemoryEventSink>) {
    let events = Arc::new(MemoryEventSink::default());
    let sink = Arc::new(MemoryAuditSink::default());
    let auditor = Auditor::new(sink.clone());
    let registry = Arc::new(Registry::new(
        ServiceHandles::default(),
        Arc::new(crate::kernel::logger::Logger),
    ));
    let dispatch = Arc::new(Dispatch::new(
        registry,
        auditor.clone(),
        Duration::from_secs(30),
        Duration::from_secs(5),
        Duration::from_secs(10 * 60),
        events.clone(),
    ));
    let loop_engine = Arc::new(
        AgentLoop::new(
            Arc::new(ScriptedLoopModel),
            dispatch,
            auditor,
            events.clone(),
            Arc::new(StubSummarizer),
            bus,
            Arc::new(|| "你是测试 Agent".to_string()),
            None,
        )
        .with_compaction_limits(context_limit, 2),
    );
    (loop_engine, sink, events)
}

fn long_messages(count: usize) -> Vec<Message> {
    (0..count)
        .map(|i| {
            Message::user(format!(
                "第 {i} 条长消息：{}",
                "数学错题讲解与知识点分析内容填充。".repeat(8)
            ))
        })
        .collect()
}

#[tokio::test]
async fn compaction_compresses_old_messages_and_keeps_tail() {
    let bus = InterruptBus::new();
    let (loop_engine, _, _) = setup_loop(bus.clone(), 100);
    let input = TurnInput {
        messages: long_messages(6),
        tools: Vec::new(),
        signal: AbortSignal::new(),
        turn_budget: Duration::from_secs(60),
        forced_tool: None,
    };
    let outcome = loop_engine.run_turn(input).await.unwrap();
    let compaction = outcome.compaction.expect("达到阈值应触发压缩");
    assert!(compaction.summarized > 0);
    assert!(
        matches!(
            outcome.messages[0].kind,
            MessageKind::System { ref text, .. } if text.contains("上下文压缩摘要")
        ),
        "摘要应作为 system 消息写入"
    );
    assert!(outcome.messages.len() <= 3, "保留摘要 + 最近 2 条");
}

#[tokio::test]
async fn below_threshold_skips_compaction() {
    let (loop_engine, _, _) = setup_loop(InterruptBus::new(), 100_000);
    let input = TurnInput {
        messages: vec![Message::user("短消息")],
        tools: Vec::new(),
        signal: AbortSignal::new(),
        turn_budget: Duration::from_secs(60),
        forced_tool: None,
    };
    let outcome = loop_engine.run_turn(input).await.unwrap();
    assert!(outcome.compaction.is_none());
}

#[tokio::test]
async fn turn_boundary_consumes_interrupts_and_audits() {
    let bus = InterruptBus::new();
    bus.send(Interrupt::ConfigChanged);
    bus.send(Interrupt::MemoryChanged {
        path: "数学/函数".into(),
    });
    let (loop_engine, sink, _) = setup_loop(bus, 100_000);
    let input = TurnInput {
        messages: vec![Message::user("你好")],
        tools: Vec::new(),
        signal: AbortSignal::new(),
        turn_budget: Duration::from_secs(60),
        forced_tool: None,
    };
    loop_engine.run_turn(input).await.unwrap();
    let records = sink.take();
    let interrupts: Vec<_> = records
        .iter()
        .filter(|r| matches!(r, AuditRecord::Interrupt { .. }))
        .collect();
    assert_eq!(interrupts.len(), 2);
    assert!(records.iter().any(|r| matches!(
        r,
        AuditRecord::Interrupt { name, .. } if name == "config_changed"
    )));
}
