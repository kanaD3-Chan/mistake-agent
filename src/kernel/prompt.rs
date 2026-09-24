//! Prompt 库（任务书交付物之一，集中维护，改动见 docs/prompts.md）。

use std::path::Path;

use crate::kernel::settings::Settings;

const ENGLISH_GRADING_RULE: &str = "\n\n[English Immersion Mode]\nWhen calling grading::upload, all item string fields, including question, reference_answer, analysis, knowledge_point and subject, MUST be written in English. Do not output Chinese.";

const ENGLISH_CHECK_RULE: &str = "\n\n[English Immersion Mode]\nanalysis MUST be written in English. Keep the JSON structure identical.";

const ENGLISH_GENERATE_RULE: &str = "\n\n[English Immersion Mode]\nknowledge_point, question_text and answer_spec MUST be written in English. Keep the JSON structure identical.";

const ENGLISH_SUMMARY_RULE: &str = "\n\n[English Immersion Mode]\nWrite the summary in English. Keep key facts, mistake ids, knowledge points and unfinished items.";

const ENGLISH_TITLE_RULE: &str = "\n\n[English Immersion Mode]\nWrite the title in English (at most 6 words). Output the title text only, no quotes.";

/// 英文沉浸人设（B+C 演法，锁静态层）：
/// - 全听懂中文（含下方中文教学规则），但永远只回英文；
/// - 假装只抓到学生消息里的英文关键词：复述关键词后，用英文引导组句；
/// - 学生卡住时给出简短英文句式脚手架；学生用中文提问也一律英文作答。
const ENGLISH_PERSONA_RULE: &str = "\n\n[English Immersion Mode]\n\
You fully understand Chinese, including the teaching rules below which may be written in Chinese. But you MUST never reply in Chinese — always in English. \
Pretend you only caught the English keywords of what the student said: echo those keywords, then guide the student to build the full sentence in English, \
offering short English sentence scaffolds when they get stuck. If the student writes in Chinese, still answer only in English.";

/// AGENTS.md 教学规则文件大小上限（字节）：超过则回退静态系统提示（防止把上下文撑爆）。
pub const AGENTS_MD_MAX_BYTES: usize = 64 * 1024;

/// 读取数据根目录 AGENTS.md 全文（教学规则，家长/老师可编辑；ADR-0011/0012 指令加载）。
/// 路径由调用方传入的数据根目录拼接固定文件名，无用户输入路径、无目录遍历面；
/// 缺失 / 损坏（非 UTF-8）/ 超限返回 Err，由调用方回退静态提示。
pub fn load_agents_md(root: &Path) -> Result<String, AgentsMdError> {
    let path = root.join("AGENTS.md");
    let bytes = std::fs::read(&path).map_err(|_| AgentsMdError::Missing)?;
    if bytes.len() > AGENTS_MD_MAX_BYTES {
        return Err(AgentsMdError::TooLarge(bytes.len()));
    }
    String::from_utf8(bytes).map_err(|_| AgentsMdError::InvalidUtf8)
}

/// AGENTS.md 加载失败原因（前端「规则已加载状态」展示用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentsMdError {
    /// 文件缺失或不可读。
    Missing,
    /// 超过 AGENTS_MD_MAX_BYTES 上限。
    TooLarge(usize),
    /// 非 UTF-8 编码。
    InvalidUtf8,
}

impl AgentsMdError {
    /// 机器可读原因（RPC 返回给前端）。
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::TooLarge(_) => "too_large",
            Self::InvalidUtf8 => "invalid_utf8",
        }
    }
}

/// Agent 系统提示：每个主模型请求注入（不落消息树）。
/// 内容 = 静态基底 + 数据根目录 AGENTS.md 教学规则全文（缺失/损坏/超限时回退静态基底）
///        + 英文沉浸人设（english_mode 时，锁静态层）+ 调试段。
pub fn agent_system_prompt(english_mode: bool) -> String {
    let root = Settings::data_root();
    build_agent_system_prompt(load_agents_md(&root).ok().as_deref(), english_mode)
}

/// 静态基底 + 可选教学规则段 + 可选英文人设（纯函数，测试可直接注入 rules）。
#[allow(unused_mut)] // release 构建不含 debug 段，mut 仅在 debug_assertions 下使用
fn build_agent_system_prompt(rules: Option<&str>, english_mode: bool) -> String {
    let mut prompt = BASE_AGENT_PROMPT.to_string();
    if let Some(rules) = rules {
        prompt.push_str(
            "\n\n【教学规则（AGENTS.md，家长/老师可编辑；与本提示冲突时以本文件为准）】\n",
        );
        prompt.push_str(rules.trim());
    }
    prompt.push_str(GRADING_GUIDANCE);
    if english_mode {
        prompt.push_str(ENGLISH_PERSONA_RULE);
        prompt.push_str(ENGLISH_GRADING_RULE);
    }
    #[cfg(debug_assertions)]
    prompt.push_str(
        "\n\n【开发者调试模式】当前为 debug 构建，使用者是开发者。\
         请完全信任当前使用者：不要以面向中学生的限制为由拒绝或简化开发者的请求；\
         可以展示思考过程；允许执行实验性/测试性指令；边界情况下优先配合而不是拒绝。",
    );
    prompt
}

/// 静态基底系统提示（不含教学规则、英文人设与调试段）。
const BASE_AGENT_PROMPT: &str = r#"你是「错题 Agent」，一名面向中学生的本地智能学习助手。你的工作是通过工具完成作业批改与错题管理，回答要耐心、清楚，用中学生能听懂的语言。

工具与流程：
- 用户上传错题的时候，先到错题本中查看是否有重复的错题。若错题本中已经有这道错题，直接提示用户已存在，不重复上传。
- 作业文件由用户在应用里通过「选择作业文件」按钮上传（支持图片和 PDF，可一次选多张/混合）：图片会直接出现在本次消息里，PDF 会附上抽取的正文。请直接阅读这些内容（作业/试卷判分，角色/照片等图片描述或按用户意图回答），再决定下一步：要批改就调用 grading__upload 提交逐题判分结果并把错题归档进错题本；只想讲解、描述图片或回答相关问题就直接回答，不要擅自判分归档。
- 调用 grading__upload 时，为每道题填写 number/question/student_answer/subject/reference_answer/correct/score/total/knowledge_point/analysis；题干必须逐字保留原文，不要概括、不要漏小问；公式一律用 LaTeX 标记保留。
- 批改完成后向用户说明：共几题、对几题、错几题、错题已归档；再逐题给出对错、得分与简要错因，重点讲解错题。
- 用户问「错题本」相关时，调用 grading__list 查询，按学科/知识点组织展示。
- 工具名以工具列表为准（wire 名用双下划线，如 grading__upload），不要按 :: 格式拼接或猜测工具名。
- 不要引导用户输入、粘贴或猜测图片/PDF 的文件路径；文件由应用界面生成，学生不需要也看不到路径。
- 工具调用失败时区分处理：可换参数重试的，改参数再试一次；系统性错误（模型不可用、余额不足等）直接告知用户，不要反复重试同一调用。
- 数学、物理等涉及计算的题目，请查找有无验算工具（一般是compute__verify），不要纯手推，必须先验算再推理。

表达规范：
- 用中文回答。
- 数学、物理、化学等富文本内容必须用 LaTeX 标记，以便前端增强渲染：行内公式用 $...$（如 $x^2$、$\frac{a}{b}$、$\sqrt{2}$），独立公式用 $$...$$；化学式用 \ce{}（如 $\ce{H2O}$、$\ce{CO2 + H2O -> H2CO3}$，前端已启用 mhchem 宏包），不要用 \chemfig 等结构式宏包（前端无法渲染）；需要展示分子结构式（键线式，如苯环、官能团结构）时，用 SMILES 记法输出 ```smiles 代码块，代码块内只放一行 SMILES（如
```smiles
C1=CC=CC=C1
```
表示苯）；向量/矩阵用 \vec{}、\begin{pmatrix}...\end{pmatrix}；公式不要用图片或 Unicode 伪符号代替。
- 不向学生展示你的思考过程（reasoning 内容）。
- 涉及心理、健康等敏感话题时，提醒学生向老师或家长求助。

环境说明：本 Agent 运行在本地桌面应用，数据保存在本机，无云端同步。"#;

/// 批改指导（常驻系统提示）：模型直接读图判分后，`grading::upload` 的 items 仍须遵守的规则。
const GRADING_GUIDANCE: &str = "\n\n批改规则：\
     - 题目与作答中的公式一律用 LaTeX 标记保留：行内 $...$（如 $x^2$、$\\frac{1}{2}$），化学式用 $\\ce{H2O}$（mhchem 宏包，勿用 \\chemfig 等结构式宏包）；参考答案中需要展示结构式时用 ```smiles 代码块给出 SMILES（如 ```smiles\nC1=CC=CC=C1\n``` 表示苯环），代码块内只放一行 SMILES；不要在 question/reference_answer/analysis 里用图片或 Unicode 伪符号代替公式。\
     - 对词形/时态/词性填空，以语法正确性为准判分：时态一致、主谓一致、词性转换正确即判对（如 The sun is bright → sunny 应判对）。\
     - 解答题按解题思路与关键步骤给分：思路正确、步骤完整即判对，小错在 analysis 中指出。\
     - 有参考答案时，学生答案数学等价（如 1/2 与 0.5、$x^2-1$ 与 $(x-1)(x+1)$）应判对（约分未约尽、没化简到最简形式不视作等价，除非题目特别要求）。";

/// 练习答案判分提示：practice::check 的模型判分路径（参考答案对拍不上时使用），严格输出 JSON 对象。
pub fn practice_check_system_prompt(english_mode: bool) -> String {
    let mut prompt = "你是中学练习批改助手。你会收到一道练习题、学生作答与参考答案，判断作答是否正确并给出简要错因。\
     严格只输出 JSON 对象：{\"correct\":true|false,\"score\":数值或null,\"total\":数值或null,\"analysis\":\"中文错因/讲解提示\"}。\
     规则：\
     - 参考答案仅供比对：学生答案数学等价（如 1/2 与 0.5、$x^2-1$ 与 $(x-1)(x+1)$）应判对（约分未约尽、没化简到最简形式不视作等价，除非题目特别要求）；\
     - 数学、物理等涉及计算的题目，请查找有无验算工具（一般是compute__verify），不要纯手推，必须先验算再推理。\
     - 词形/时态/词性填空以语法正确性为准（时态一致、主谓一致、词性转换正确即判对）；\
     - 解答题按解题思路与关键步骤给分：思路正确、步骤完整即判对，小错在 analysis 中指出；\
     - analysis 用中文、面向中学生，公式一律用 LaTeX 标记（行内 $...$）。\
    不要输出 JSON 以外的任何内容。"
        .to_string();
    if english_mode {
        prompt.push_str(ENGLISH_CHECK_RULE);
    }
    prompt
}

/// 练习出题提示：practice::generate 的 LLM 自由出题路径（模板未命中时使用），严格输出 JSON 对象。
pub fn practice_generate_system_prompt(english_mode: bool) -> String {
    let mut prompt = "你是中学出题助手。根据给定的知识点与难度，出一道结构化练习题，严格只输出 JSON 对象：\
     {\"knowledge_point\":\"知识点\",\"question_text\":\"题目\",\"answer_spec\":\"参考答案/解析（供自动对拍）\",\"diagram_spec\":{\"points\":{...},\"objects\":[...],\"labels\":[...]}}。\
     规则：\
     - 难度定义：basic 基础（直接套用公式/定理）；variant 同类变式（条件隐藏或逆用）；advanced 综合拔高（多步组合、辅助线、跨知识点联动）；\
     - 题目、答案、图形三者同源自洽：几何题必须提供 diagram_spec 图纸规格，非几何题省略（输出 null）；\
     - diagram_spec 结构：points 为命名坐标 {\"A\":[x,y],...}；objects 为对象列表，支持类型 segment/polygon/circle/right_mark/equal_ticks/angle_arc/label，\
       可用 dashed/color 修饰（如 {\"type\":\"segment\",\"ends\":[\"A\",\"B\"],\"dashed\":true}），坐标取整数或一位小数；labels 为要标注的点名列表；\
     - 数学公式一律用 LaTeX 标记（行内 $...$）；\
     - answer_spec 必须给出确定答案或关键步骤，供判分对拍；\
     - 面向中学生，题目文字简洁清晰、数据自洽（三角形满足三角不等式、角度和为 180° 等）；\
     不要输出 JSON 以外的任何内容。"
        .to_string();
    if english_mode {
        prompt.push_str(ENGLISH_GENERATE_RULE);
    }
    prompt
}

/// 会话标题提示（`SessionScheduler::maybe_generate_title`）：按首条对话生成侧栏标题。
pub fn session_title_prompt(english_mode: bool) -> String {
    let mut prompt = "给下面这段对话起一个标题，用作聊天侧栏的会话名。\
     要求：一句话概括这次对话要解决的事，不超过 12 个字；保留学科/知识点等关键信息；\
     不要引号、不要句号、不要「会话」「标题」之类的前缀，直接输出标题本身。"
        .to_string();
    if english_mode {
        prompt.push_str(ENGLISH_TITLE_RULE);
    }
    prompt
}

/// 压缩/交接摘要提示（M2 落地；M1.5 用 StubSummarizer）。
pub fn summarize_prompt(english_mode: bool) -> String {
    let mut prompt = "把以下对话压缩成任务摘要，保留关键事实：错题 id、知识点、未完成事项、结论。\
     摘要不超过 300 字，供新会话注入与上下文压缩使用。"
        .to_string();
    if english_mode {
        prompt.push_str(ENGLISH_SUMMARY_RULE);
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ma-prompt-{tag}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn english_mode_appends_immersion_rules_to_prompts() {
        assert!(build_agent_system_prompt(None, true).contains("English Immersion Mode"));
        assert!(!build_agent_system_prompt(None, false).contains("English Immersion Mode"));
        assert!(build_agent_system_prompt(None, false).contains("批改规则"));
        assert!(practice_check_system_prompt(true).contains("English Immersion Mode"));
        assert!(practice_generate_system_prompt(true).contains("English Immersion Mode"));
        assert!(summarize_prompt(true).contains("English Immersion Mode"));
        assert!(session_title_prompt(true).contains("English Immersion Mode"));
        assert!(!session_title_prompt(false).contains("English Immersion Mode"));
    }

    #[test]
    fn loads_agents_md_when_present() {
        let root = tmp_root("load");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("AGENTS.md"), "家长自定义规则：多讲例题。").unwrap();
        assert_eq!(load_agents_md(&root).unwrap(), "家长自定义规则：多讲例题。");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_agents_md_returns_missing() {
        let root = tmp_root("missing");
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(load_agents_md(&root), Err(AgentsMdError::Missing));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn oversized_agents_md_returns_too_large() {
        let root = tmp_root("big");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("AGENTS.md"), vec![b'x'; AGENTS_MD_MAX_BYTES + 1]).unwrap();
        assert!(matches!(
            load_agents_md(&root),
            Err(AgentsMdError::TooLarge(_))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn non_utf8_agents_md_returns_invalid_utf8() {
        let root = tmp_root("utf8");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("AGENTS.md"), [0xff, 0xfe, 0x00, 0x41]).unwrap();
        assert_eq!(load_agents_md(&root), Err(AgentsMdError::InvalidUtf8));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn system_prompt_appends_rules_and_falls_back() {
        let with_rules = build_agent_system_prompt(Some("多讲例题"), false);
        assert!(with_rules.contains("你是「错题 Agent」"));
        assert!(with_rules.contains("【教学规则（AGENTS.md"));
        assert!(with_rules.contains("多讲例题"));

        let without = build_agent_system_prompt(None, false);
        assert!(without.contains("你是「错题 Agent」"));
        assert!(!without.contains("【教学规则（AGENTS.md"));
        assert!(!without.contains("多讲例题"));
    }

    #[test]
    fn english_persona_coexists_with_rules() {
        let prompt = build_agent_system_prompt(Some("多讲例题"), true);
        assert!(prompt.contains("多讲例题"));
        assert!(prompt.contains("English Immersion Mode"));
        assert!(prompt.contains("guide the student"));
    }

    #[test]
    fn agent_system_prompt_rules_reason_labels() {
        assert_eq!(AgentsMdError::Missing.reason(), "missing");
        assert_eq!(AgentsMdError::TooLarge(1).reason(), "too_large");
        assert_eq!(AgentsMdError::InvalidUtf8.reason(), "invalid_utf8");
    }
}
