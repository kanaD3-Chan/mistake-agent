-- 0003_turn_billing：回合制计费（ADR-0047 修订 R14）
--
-- 背景（实测）：学生只发一句 "hello"，服务端却产生了 3 条流水、扣了 3 次 ——
--   1) 主回合第一轮（Agent loop 的模型往返）
--   2) 工具轮之后的第二次往返（同一上下文，缓存命中）
--   3) 会话标题生成（`session::title` 是一次独立的模型请求）
-- 根因是"每一次模型往返都各扣一次"，而学生感知的是"我问了一句话"。
--
-- 修法：给流水加 turn_id（客户端为每个用户回合生成）。同一回合**只在第一条流水上记费**，
-- 其余流水 billed_uses = 0（token 明细照记，内账完整）；阶梯改为按**回合累计 token**
-- 决定 1/2/3 次。防滥用由 store::reserve 的窗口（15 分钟）与条数上限（12 条）兜住。
--
-- 兼容：turn_id 为 NULL 的流水（老客户端 / 未带回合标识）= 该行自成一回合，行为与改动前一致。

ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS turn_id text;

COMMENT ON COLUMN usage_events.turn_id IS
    '客户端为每个用户回合生成的标识（ADR-0047 修订 R14）；NULL = 该行自成一回合';

-- 结算时按回合重算扣次是热路径
CREATE INDEX IF NOT EXISTS usage_events_user_turn_idx
    ON usage_events (user_id, turn_id)
    WHERE turn_id IS NOT NULL;
