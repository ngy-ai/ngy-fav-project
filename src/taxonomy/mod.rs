//! 分类与标签。
//!
//! 词表（有哪些分类、哪些标签）由 `Library` 统一持有，这里只放两样东西：
//! `model` 是名字规则的纯逻辑，`chip` 是它的色块渲染，`taxonomy_view` 是管理页。
//!
//! 归属关系按需求定死：**一个仓库最多属于一个分类，可以挂任意多个标签**。
//! 所以在 `Repo` 上 `category` 是 `Option<String>`，`tags` 是 `Vec<String>`。

pub mod chip;
pub mod model;

mod taxonomy_view;

pub use model::TaxonomyKind;
pub use taxonomy_view::TaxonomyView;
