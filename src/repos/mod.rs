//! 仓库页：列表 + 详情，以及添加 / 编辑 / 移除仓库的对话框。

pub mod model;

mod add_repo_dialog;
mod bulk_category_dialog;
mod edit_description_dialog;
mod list_delegate;
mod repos_view;
mod taxonomy_dialog;

pub use list_delegate::{RepoListDelegate, SelectAll};
pub use model::{CategoryFilter, Repo, RepoId, RepoRow, parse_full_name};
pub use repos_view::ReposView;
