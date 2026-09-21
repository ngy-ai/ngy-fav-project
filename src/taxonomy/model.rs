//! 分类与标签的纯逻辑。
//!
//! 两者共用同一套「名字」规则：名字就是身份，比较时忽略大小写，存下来的是第一次
//! 出现的写法。区别只在归属关系——一个仓库最多属于一个分类，可以挂任意多个标签。
//!
//! 这里不碰 `Library`、也不碰界面：输入字符串、输出判断，好测。

use std::collections::HashSet;

/// 名字的长度上限。它是一行给人看的字，超过这个长度一定是误输入。
pub const MAX_NAME_CHARS: usize = 40;

/// 词表的两个种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaxonomyKind {
    Category,
    Tag,
}

impl TaxonomyKind {
    /// 界面上称呼它的词。
    pub fn label(self) -> &'static str {
        match self {
            Self::Category => "分类",
            Self::Tag => "标签",
        }
    }
}

/// 加名字的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddName {
    Added,
    /// 已经有同名（忽略大小写）的一项了，什么都没改。
    AlreadyPresent,
    /// 空名字或超过 [`MAX_NAME_CHARS`]。
    Invalid,
}

/// 改名字的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameName {
    Renamed,
    /// 新旧名字算同一个，什么都没动，原有写法也留着。
    Unchanged,
    /// 目标名字已经被别人占了。
    NameTaken,
    /// 要改的那个名字不在词表里。
    NotFound,
    /// 新名字不合法。
    Invalid,
}

/// 收干净用户输入的名字。返回 `None` 表示这个名字不能用。
///
/// 首尾空白去掉、内部连续空白压成一个空格——`"AI  工具"` 与 `" AI 工具 "`
/// 是同一个人想说的同一件事。
pub fn clean_name(raw: &str) -> Option<String> {
    let cleaned = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() || cleaned.chars().count() > MAX_NAME_CHARS {
        return None;
    }
    Some(cleaned)
}

/// 名字的身份比较：忽略大小写。`AI` 和 `ai` 是同一个分类。
pub fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// 在词表里找名字，返回它的下标。
pub fn index_of(names: &[String], name: &str) -> Option<usize> {
    names.iter().position(|existing| same_name(existing, name))
}

pub fn contains(names: &[String], name: &str) -> bool {
    index_of(names, name).is_some()
}

/// 追加一个名字。已经有同名的就什么都不做并返回 `false`，原有写法保留。
pub fn push_new(names: &mut Vec<String>, raw: &str) -> bool {
    let Some(name) = clean_name(raw) else {
        return false;
    };
    if contains(names, &name) {
        return false;
    }
    names.push(name);
    true
}

/// 词表自身去重，保留第一次出现的写法。手改过 `library.json` 之后用得上。
/// 返回是否真的去掉过东西。
pub fn dedupe(names: &mut Vec<String>) -> bool {
    let before = names.len();
    let mut seen = HashSet::new();
    names.retain(|name| seen.insert(name.to_lowercase()));
    names.len() != before
}

/// 分类的色相（0..360）。同一个名字永远得到同一个色相，相邻的名字也不会挤在
/// 一起——不为好看，只为「一眼认出是不是同一类」。
pub fn hue_seed(name: &str) -> u16 {
    // FNV-1a：短字符串上分布够匀，实现短到不用引依赖。
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.to_lowercase().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    // 乘一个和 360 互质的步进量，让名字列表里相邻的两项落到不同色相上。
    ((hash.wrapping_mul(137)) % 360) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn clean_name_trims_and_squeezes_whitespace() {
        assert_eq!(clean_name("  工具  ").as_deref(), Some("工具"));
        assert_eq!(clean_name("AI\t工具").as_deref(), Some("AI 工具"));
        assert_eq!(clean_name("周榜 ·  全部语言").as_deref(), Some("周榜 · 全部语言"));
    }

    #[test]
    fn clean_name_rejects_empty_and_overlong_input() {
        assert_eq!(clean_name(""), None);
        assert_eq!(clean_name("   "), None);
        assert_eq!(clean_name("　"), None, "全角空格也是空白");
        assert_eq!(clean_name(&"x".repeat(MAX_NAME_CHARS)), Some("x".repeat(MAX_NAME_CHARS)));
        assert_eq!(clean_name(&"x".repeat(MAX_NAME_CHARS + 1)), None);
    }

    #[test]
    fn name_identity_ignores_case() {
        assert!(same_name("AI", "ai"));
        assert!(same_name("Rust", "rUsT"));
        assert!(!same_name("AI", "AI 工具"));
    }

    #[test]
    fn push_new_keeps_the_first_spelling() {
        let mut list = names(&["AI"]);
        assert!(!push_new(&mut list, "ai"), "同名不该重复添加");
        assert_eq!(list, names(&["AI"]), "第一次的写法要留着");

        assert!(push_new(&mut list, " 命令行 "));
        assert_eq!(list, names(&["AI", "命令行"]));

        assert!(!push_new(&mut list, "   "), "不能用的名字不该进词表");
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn index_of_uses_the_same_identity_as_push_new() {
        let list = names(&["AI", "命令行"]);
        assert_eq!(index_of(&list, "ai"), Some(0));
        assert_eq!(index_of(&list, "命令行"), Some(1));
        assert_eq!(index_of(&list, "没有"), None);
    }

    #[test]
    fn dedupe_keeps_the_first_of_each_spelling() {
        let mut list = names(&["AI", "ai", "命令行", "命令行"]);
        assert!(dedupe(&mut list), "去掉过东西就要说去掉过");
        assert_eq!(list, names(&["AI", "命令行"]));

        assert!(!dedupe(&mut list), "已经是干净的就不该再报改动");
    }

    #[test]
    fn hue_seed_is_stable_in_range_and_spread_out() {
        let samples = ["AI", "工具", "命令行", "Web", "数据库", "游戏", "安全", "文档"];
        let hues: Vec<u16> = samples.iter().map(|name| hue_seed(name)).collect();

        for hue in &hues {
            assert!(*hue < 360, "色相必须落在 0..360，得到 {hue}");
        }
        // 同一个名字每次都得到同一个色相，否则颜色会随重绘乱跳。
        assert_eq!(hue_seed("AI"), hue_seed("AI"));
        assert_eq!(hue_seed("AI"), hue_seed("ai"), "大小写不该换颜色");

        let unique: HashSet<u16> = hues.iter().copied().collect();
        assert!(
            unique.len() >= samples.len() - 1,
            "8 个常见分类最多允许撞色一次：{hues:?}"
        );
    }
}
