//! 应用编排层：连接编辑核心、共享协议和前端抽象，不依赖具体 TUI/GUI 实现。
//!
//! `application` 定义稳定的 `App` 入口；`runtime` 负责事件循环；`kernel` 和 `session`
//! 分别维护后端任务/保存状态与客户端 Scene/View 状态；`layout` 和 `query` 提供布局入口
//! 与前端查询适配。

mod application;
#[cfg(test)]
mod behavior;
mod bootstrap;
mod buffer_lifecycle;
mod command_resolver;
mod content_classifier;
mod diagnostics;
mod dispatcher;
mod execution;
mod kernel;
mod layout;
mod message;
mod mode_resolver;
mod native_commands;
mod operation;
mod query;
mod remote;
mod runtime;
mod save;
mod scene_model;
mod session;
mod tasks;
mod theme;
mod transaction;
mod view;
mod view_context;
mod view_definition;
mod view_extension;
mod view_workspace;

pub(crate) use vell_mode as mode;
pub(crate) use vell_mode::{action, command, mode_name, presentation};

pub use application::App;
pub use buffer_lifecycle::{BufferInfo, BufferLifecycleError};
pub use diagnostics::{
    ModeDecorationDiagnostics, NamedPolicySources, RuntimeDiagnostic, ViewModeDiagnostics,
};
pub use layout::{StatusBarHandle, StatusBarPlacement};
pub use native_commands::native_command_ids;
pub use vell_mode::{FaceRegistrationError, FaceRegistrationErrorReason};
pub use view::{BODY_PANE, PaneKey, STATUS_PANE, View, ViewPaneMap};

#[cfg(test)]
mod tests;
