//! 分类与标签。
//!
//! 词表由 `Library` 统一持有，这里放三块纯逻辑加一个页面：
//! `model` 是名字与路径语法的规则，`tree` 是分类树本身，`chip` 是色块渲染，
//! `taxonomy_view` 是管理页。
//!
//! 归属关系按需求定死：**一个仓库最多属于一个分类，可以挂任意多个标签**。
//! 分类是树，所以 `Repo` 上记的是 `category_id`；标签是扁平的，记的是名字。

pub mod chip;
pub mod model;
pub mod tree;

mod taxonomy_view;

pub use model::TaxonomyKind;
pub use taxonomy_view::TaxonomyView;
