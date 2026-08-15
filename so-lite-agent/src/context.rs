//! 插件注册上下文：两段式契约第二阶段（注入句柄 + 绑定 handler）。

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use crate::contract::{CallerPolicy, Info, PluginError, full_name, full_to_wire};
use crate::dispatch::{CommandHandler, EventHandler, ToolHandler};
use crate::registry::{EntryKind, Handler, RegisteredEntry};
use crate::services::ServiceHandles;

pub struct RegistrarTargets<'a> {
    pub handlers: &'a RwLock<HashMap<String, RegisteredEntry>>,
    pub wire_to_full: &'a RwLock<HashMap<String, String>>,
}

pub struct EntryRegistrar<'a> {
    namespace: &'a str,
    declared: &'a Info,
    targets: RegistrarTargets<'a>,
}

impl<'a> EntryRegistrar<'a> {
    pub fn new(namespace: &'a str, declared: &'a Info, targets: RegistrarTargets<'a>) -> Self {
        Self {
            namespace,
            declared,
            targets,
        }
    }

    pub fn tool(&self, short: &str, handler: ToolHandler) -> Result<(), PluginError> {
        let def = self
            .declared
            .tools
            .iter()
            .find(|t| t.name == short)
            .ok_or_else(|| PluginError::UndeclaredEntry(short.into()))?;
        let full = full_name(self.namespace, short);
        self.insert(
            full,
            RegisteredEntry {
                full_name: full_name(self.namespace, short),
                kind: EntryKind::Tool,
                policy: def.policy,
                timeout: def.timeout.map(Duration::from_secs),
                description: def.description.clone(),
                icon: def.icon.clone(),
                params: def.params.clone(),
                handler: Handler::Tool(handler),
            },
        )
    }

    pub fn command(&self, short: &str, handler: CommandHandler) -> Result<(), PluginError> {
        let def = self
            .declared
            .commands
            .iter()
            .find(|c| c.name == short)
            .ok_or_else(|| PluginError::UndeclaredEntry(short.into()))?;
        self.insert(
            full_name(self.namespace, short),
            RegisteredEntry {
                full_name: full_name(self.namespace, short),
                kind: EntryKind::Command,
                policy: CallerPolicy::UserOnly,
                timeout: None,
                description: def.description.clone(),
                icon: def.icon.clone(),
                params: def.params.clone(),
                handler: Handler::Command(handler),
            },
        )
    }

    pub fn event(&self, name: &str, handler: EventHandler) -> Result<(), PluginError> {
        if !self.declared.events.iter().any(|e| e.name == name) {
            return Err(PluginError::UndeclaredEntry(name.into()));
        }
        self.insert(
            full_name(self.namespace, name),
            RegisteredEntry {
                full_name: full_name(self.namespace, name),
                kind: EntryKind::Event,
                policy: CallerPolicy::UserOnly,
                timeout: None,
                description: String::new(),
                icon: None,
                params: crate::contract::empty_params(),
                handler: Handler::Event(handler),
            },
        )
    }

    fn insert(&self, full: String, entry: RegisteredEntry) -> Result<(), PluginError> {
        let mut handlers = self.targets.handlers.write().expect("registry poisoned");
        if handlers.contains_key(&full) {
            return Err(PluginError::DuplicateEntry(full));
        }
        handlers.insert(full.clone(), entry);
        self.targets
            .wire_to_full
            .write()
            .expect("registry poisoned")
            .insert(full_to_wire(&full), full);
        Ok(())
    }
}

pub struct PluginContext<'a> {
    pub handles: ServiceHandles,
    pub registrar: EntryRegistrar<'a>,
}

pub struct KernelContext<'a> {
    pub handles: ServiceHandles,
    pub registrar: EntryRegistrar<'a>,
}
