//! 应用编排层：连接编辑核心、共享协议和前端抽象，不依赖具体 TUI/GUI 实现。
//!
//! `application` 定义稳定的 `App` 入口；`runtime` 负责事件循环；`kernel` 和 `session`
//! 分别维护后端任务/保存状态与客户端 Scene/View 状态；`layout` 和 `query` 提供布局入口
//! 与前端查询适配。

mod application;
#[cfg(test)]
mod behavior;
mod bootstrap;
mod command_resolver;
mod diagnostics;
mod dispatcher;
mod execution;
mod kernel;
mod layout;
mod message;
mod operation;
mod query;
mod remote;
mod runtime;
mod save;
mod scene_model;
mod session;
mod tasks;
mod transaction;
mod view;

pub(crate) use vell_mode as mode;
pub(crate) use vell_mode::{action, command, mode_name, presentation};

pub use application::App;
pub use diagnostics::{ModeDecorationDiagnostics, NamedPolicySources, ViewModeDiagnostics};

#[cfg(test)]
mod tests;
