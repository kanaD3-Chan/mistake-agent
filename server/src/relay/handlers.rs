//! 中转处理函数：一次请求的完整生命周期（ADR-0047 决策 5，修订 R2/R3/R5）。
//!
//! 顺序固定为 **鉴权 → 并发闸门 → 预扣 → 转发 → 流式 tee → 结算**：
//! 预扣在转发之前（额度不够就不该消耗上游），结算在流结束之后（真实用量只有那时才知道）。

use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::billing::{self, ReserveOutcome, Settlement, TokenUsage, UsageStatus, billed_uses};
use crate::http::AppState;
use crate::security::ClientIp;

use super::error::RelayError;
use super::protocol::Protocol;
use super::sse::{ProbeResult, Terminal, UsageProbe};
use super::upstream;

/// SSE 通道容量：够吸收网络抖动，又不至于在"客户端读得极慢"时无限吃内存。
const SSE_CHANNEL_CAPACITY: usize = 16;

pub async fn relay(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    auth: AuthUser,
    client: ClientIp,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, RelayError> {
    let protocol = Protocol::from_path(uri.path()).ok_or(RelayError::UnknownPath)?;

    let mut payload: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| RelayError::InvalidJson)?;
    if !protocol.stream_requested(&payload) {
        return Err(RelayError::StreamRequired);
    }
    // 不信任客户端传来的模型名；注入平台用户 id 换取上游的隔离能力（R5）
    protocol.apply_model(&mut payload, &state.config.deepseek_model);
    protocol.apply_user_id(&mut payload, &auth.user.id.to_string());

    // 令牌桶（按用户 + 按 IP）：管**持续速率**
    state
        .security
        .check_relay_rate(auth.user.id, &client.0)
        .map_err(|retry_after| RelayError::RateLimited {
            scope: "relay",
            retry_after_secs: retry_after.as_secs().max(1),
        })?;

    // 两道并发闸门：每用户（防单账号刷爆）与全局（保护上游账号与进程容量）。
    // 它们的名额**必须活到流结束**——见下面 spawn 里的接管，否则限额只覆盖到"建连"阶段。
    let limit = state.config.relay_max_concurrent_per_user;
    let gate = state
        .relay_gate
        .acquire(auth.user.id, limit)
        .ok_or(RelayError::TooManyConcurrent { limit })?;
    let global_slot = state
        .security
        .acquire_global_slot()
        .ok_or(RelayError::GlobalBusy {
            retry_after_secs: 5,
        })?;

    let request_id = Uuid::new_v4().to_string();
    let started = Instant::now();

    // 回合标识（ADR-0047 修订 R14）：客户端为每个用户消息生成一次，同一回合里的工具往返
    // 与会话标题等辅助调用沿用同一个值——服务端据此把"一次提问"合并成一次扣次。
    // 没有它（老客户端/第三方客户端）就退回"一个请求一个回合"，行为与改动前一致。
    let turn_id = headers
        .get("x-ma-turn-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 64)
        .map(str::to_string);

    // ---------- 预扣（R2）：额度不足就不该消耗上游 ----------
    let reservation = billing::reserve(
        &state.pool,
        auth.user.id,
        auth.token_id,
        &request_id,
        turn_id.as_deref(),
        protocol.as_str(),
        &state.config.deepseek_model,
    )
    .await?;
    let (event_id, entitlement_id) = match reservation {
        ReserveOutcome::Denied(denial) => return Err(RelayError::Quota(denial)),
        ReserveOutcome::Granted {
            event_id,
            entitlement_id,
            ..
        } => (event_id, entitlement_id),
    };

    // ---------- 转发 ----------
    let upstream_response = match upstream::forward(
        &state.upstream,
        &state.config.deepseek_base_url,
        &state.config.deepseek_api_key,
        protocol,
        &payload,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            // 上游连不上：退回预扣（该用户这次不该被计次）
            settle_once(
                &state,
                event_id,
                entitlement_id,
                Settlement {
                    status: UsageStatus::UpstreamError,
                    billed_uses: 0,
                    usage: TokenUsage::default(),
                    latency_ms: latency_ms(started.elapsed()),
                },
            )
            .await;
            return Err(error);
        }
    };

    // ---------- 上游报错：状态码与响应体原样透传，不扣次 ----------
    if !upstream_response.status().is_success() {
        let status = upstream_response.status();
        let text = upstream_response.text().await.unwrap_or_default();
        settle_once(
            &state,
            event_id,
            entitlement_id,
            Settlement {
                status: UsageStatus::UpstreamError,
                billed_uses: 0,
                usage: TokenUsage::default(),
                latency_ms: latency_ms(started.elapsed()),
            },
        )
        .await;
        tracing::warn!(user_id = %auth.user.id, %status, "上游返回错误，原样透传且不计次");
        let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        return Ok((status, text).into_response());
    }

    // ---------- 流式 tee：原样转发给客户端，旁路攒 usage ----------
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(SSE_CHANNEL_CAPACITY);
    let pool = state.pool.clone();
    let ladder = state.config.billing_ladder_tokens.clone();
    let user_id = auth.user.id;
    let model = state.config.deepseek_model.clone();

    tokio::spawn(async move {
        // 接管两道的并发名额：handler 把响应交出去就返回了，而请求其实还在飞——
        // 名额必须活到流结束（无论正常结束、出错还是客户端断开）
        let _gate = gate;
        let _global_slot = global_slot;
        let mut probe = UsageProbe::new(protocol);
        let mut stream = upstream_response.bytes_stream();
        let mut client_gone = false;

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    probe.push(&bytes);
                    if tx.send(Ok(bytes)).await.is_err() {
                        // 客户端断开：跳出后上游流被 drop，上游请求随之取消
                        client_gone = true;
                        break;
                    }
                }
                Err(error) => {
                    tracing::warn!(error = %error, user_id = %user_id, "上游流中断");
                    break;
                }
            }
        }

        let result = probe.finish();
        let settlement = settlement_for(&result, &ladder, started.elapsed());
        match billing::settle(&pool, event_id, entitlement_id, settlement, &ladder).await {
            Ok(Some(billed_uses)) => tracing::info!(
                user_id = %user_id,
                event_id,
                model = %model,
                protocol = protocol.as_str(),
                status = settlement.status.as_str(),
                billed_uses,
                input_tokens = settlement.usage.input_total,
                cached_tokens = settlement.usage.cached,
                output_tokens = settlement.usage.output,
                reasoning_tokens = settlement.usage.reasoning,
                latency_ms = settlement.latency_ms,
                client_gone,
                "中转用量已结算"
            ),
            Ok(None) => tracing::warn!(event_id, "该流水已被结算过，跳过"),
            Err(error) => tracing::error!(
                error = %error, event_id, user_id = %user_id,
                "用量结算失败：账目可能不准，需人工核对"
            ),
        }
    });

    let mut response = Response::new(Body::from_stream(ReceiverStream::new(rx)));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(response)
}

/// 由旁路解析结果判定扣次（R2 结算 + R3 中断规则）。
///
/// 注意（R14 回合制）：这里算的是**单个请求**的阶梯值，仅作日志与兜底参考；
/// 对外扣次的最终归属由 `store::recompute_turn_charge` 按**回合累计 token** 决定。
fn settlement_for(result: &ProbeResult, ladder: &[u64], elapsed: Duration) -> Settlement {
    let latency_ms = latency_ms(elapsed);
    let usage = result.usage.unwrap_or_default();

    // 上游明确报失败：不计次（预留会被退回）
    if result.terminal == Terminal::Failed {
        return Settlement {
            status: UsageStatus::UpstreamError,
            billed_uses: 0,
            usage,
            latency_ms,
        };
    }

    // 拿到 usage：账是完整的，按阶梯结算；客户端在此期间断开也照样如实记账
    if let Some(usage) = result.usage {
        return Settlement {
            status: UsageStatus::Ok,
            billed_uses: billed_uses(usage.billable_tokens(), ladder),
            usage,
            latency_ms,
        };
    }

    // 没拿到 usage：只要上游已经开始生成（看到过事件）就至少计 1 次——
    // 上游一旦开始生成就已经为 prompt 计费了（R3）；一个事件都没有才不计。
    Settlement {
        status: UsageStatus::Aborted,
        billed_uses: u32::from(result.saw_event),
        usage,
        latency_ms,
    }
}

async fn settle_once(
    state: &AppState,
    event_id: i64,
    entitlement_id: Uuid,
    settlement: Settlement,
) {
    let ladder = &state.config.billing_ladder_tokens;
    if let Err(error) =
        billing::settle(&state.pool, event_id, entitlement_id, settlement, ladder).await
    {
        tracing::error!(
            error = %error, event_id,
            "用量结算失败（失败路径）：账目可能不准，需人工核对"
        );
    }
}

fn latency_ms(elapsed: Duration) -> i32 {
    i32::try_from(elapsed.as_millis()).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billing::TokenUsage;

    fn result(usage: Option<TokenUsage>, saw_event: bool, terminal: Terminal) -> ProbeResult {
        ProbeResult {
            usage,
            saw_event,
            saw_output: saw_event,
            terminal,
        }
    }

    const LADDER: [u64; 2] = [32 * 1024, 64 * 1024];

    #[test]
    fn settled_usage_bills_by_ladder() {
        let usage = TokenUsage {
            input_total: 40 * 1024,
            cached: 0,
            output: 1024,
            reasoning: 0,
        };
        let settlement = settlement_for(
            &result(Some(usage), true, Terminal::Completed),
            &LADDER,
            Duration::from_millis(5),
        );
        assert_eq!(settlement.status, UsageStatus::Ok);
        assert_eq!(settlement.billed_uses, 2, "40k+1k 跨过 32k 阈值 → 2 次");
    }

    #[test]
    fn explicit_upstream_failure_costs_nothing() {
        let settlement = settlement_for(
            &result(Some(TokenUsage::default()), true, Terminal::Failed),
            &LADDER,
            Duration::from_millis(5),
        );
        assert_eq!(settlement.status, UsageStatus::UpstreamError);
        assert_eq!(settlement.billed_uses, 0, "上游明确失败不扣次");
    }

    #[test]
    fn interrupted_stream_bills_the_minimum_when_output_started() {
        // 没有 usage、没有终态，但已看到输出：上游已开始生成，收最低 1 次（R3）
        let settlement = settlement_for(
            &result(None, true, Terminal::Pending),
            &LADDER,
            Duration::from_millis(5),
        );
        assert_eq!(settlement.status, UsageStatus::Aborted);
        assert_eq!(settlement.billed_uses, 1);
    }

    #[test]
    fn stream_that_never_started_costs_nothing() {
        let settlement = settlement_for(
            &result(None, false, Terminal::Pending),
            &LADDER,
            Duration::from_millis(5),
        );
        assert_eq!(settlement.status, UsageStatus::Aborted);
        assert_eq!(settlement.billed_uses, 0, "一个事件都没有就不该计次");
    }
}
