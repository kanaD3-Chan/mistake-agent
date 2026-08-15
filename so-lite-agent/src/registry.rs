//! 注册表：启动 fail-fast 校验、两段式契约、懒注册、模型工具列表过滤。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use schemars::Schema;

use crate::context::{EntryRegistrar, KernelContext, PluginContext, RegistrarTargets};
use crate::contract::{
    CallerPolicy, Info, LoadPolicy, PluginError, ToolDef, full_name, full_to_wire,
};
use crate::dispatch::{CommandHandler, EventHandler, ToolHandler};
use crate::services::{ServiceHandles, ServiceId, ToolSchema};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Tool,
    Command,
    Event,
}

#[derive(Clone)]
pub enum Handler {
    Tool(ToolHandler),
    Command(CommandHandler),
    Event(EventHandler),
}

#[derive(Clone)]
pub struct RegisteredEntry {
    pub full_name: String,
    pub kind: EntryKind,
    pub policy: CallerPolicy,
    pub timeout: Option<Duration>,
    pub description: String,
    pub icon: Option<String>,
    pub params: Schema,
    pub handler: Handler,
}

pub struct PluginDescriptor {
    pub info: Info,
    pub register: fn(PluginContext<'_>) -> Result<(), PluginError>,
}

impl PluginDescriptor {
    pub fn from_plugin<P: UserPlugin>() -> Self {
        Self {
            info: P::info(),
            register: P::register,
        }
    }
}

pub trait UserPlugin {
    fn info() -> Info;
    fn register(ctx: PluginContext<'_>) -> Result<(), PluginError>;
}

pub struct KernelDescriptor {
    pub info: Info,
    pub register: fn(KernelContext<'_>) -> Result<(), PluginError>,
}

impl KernelDescriptor {
    pub fn from_plugin<P: KernelPlugin>() -> Self {
        Self {
            info: P::info(),
            register: P::register,
        }
    }
}

pub trait KernelPlugin {
    fn info() -> Info;
    fn register(ctx: KernelContext<'_>) -> Result<(), PluginError>;
}

enum PluginBody {
    User(fn(PluginContext<'_>) -> Result<(), PluginError>),
    Kernel(fn(KernelContext<'_>) -> Result<(), PluginError>),
}

struct PluginEntry {
    info: Info,
    body: PluginBody,
    loaded: AtomicBool,
}

pub struct Registry {
    entries: RwLock<HashMap<String, Arc<PluginEntry>>>,
    handlers: RwLock<HashMap<String, RegisteredEntry>>,
    wire_to_full: RwLock<HashMap<String, String>>,
    services: ServiceHandles,
}

impl Registry {
    pub fn new(services: ServiceHandles) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            handlers: RwLock::new(HashMap::new()),
            wire_to_full: RwLock::new(HashMap::new()),
            services,
        }
    }

    pub fn register_plugin(&self, desc: PluginDescriptor) -> Result<(), PluginError> {
        self.register_inner(desc.info, PluginBody::User(desc.register))
    }

    pub fn register_kernel_plugin(&self, desc: KernelDescriptor) -> Result<(), PluginError> {
        self.register_inner(desc.info, PluginBody::Kernel(desc.register))
    }

    fn register_inner(&self, info: Info, body: PluginBody) -> Result<(), PluginError> {
        let is_kernel = matches!(&body, PluginBody::Kernel(_));
        {
            let entries = self.entries.read().expect("registry poisoned");
            if entries.contains_key(&info.namespace) {
                return Err(PluginError::NamespaceTaken(info.namespace.clone()));
            }
        }

        if is_kernel {
            let mut provided: HashSet<ServiceId> = HashSet::new();
            {
                let entries = self.entries.read().expect("registry poisoned");
                for e in entries.values() {
                    provided.extend(e.info.provides.iter().copied());
                }
            }
            for id in &info.provides {
                if !provided.insert(*id) {
                    return Err(PluginError::ServiceTaken(*id));
                }
            }
        } else if !info.provides.is_empty() {
            return Err(PluginError::ProvisionNotAllowed(info.provides.clone()));
        }

        if !is_kernel {
            let available = self.services.available();
            let missing: Vec<_> = info
                .requires
                .iter()
                .filter(|r| !available.contains(r))
                .copied()
                .collect();
            if !missing.is_empty() {
                return Err(PluginError::CapabilityUnavailable(missing));
            }
        }

        let mut fulls: HashSet<String> = HashSet::new();
        let mut wires: HashSet<String> = HashSet::new();
        {
            let entries = self.entries.read().expect("registry poisoned");
            for e in entries.values() {
                for t in &e.info.tools {
                    wires.insert(full_to_wire(&full_name(&e.info.namespace, &t.name)));
                }
                for c in &e.info.commands {
                    wires.insert(full_to_wire(&full_name(&e.info.namespace, &c.name)));
                }
                for ev in &e.info.events {
                    wires.insert(full_to_wire(&full_name(&e.info.namespace, &ev.name)));
                }
            }
        }
        let mut check_name = |short: &str, kind: &str| -> Result<(), PluginError> {
            let full = full_name(&info.namespace, short);
            let wire = full_to_wire(&full);
            if !fulls.insert(full.clone()) {
                return Err(PluginError::DuplicateEntry(full));
            }
            if !wires.insert(wire.clone()) {
                return Err(PluginError::WireNameCollision(format!(
                    "{kind} {short} → {wire}"
                )));
            }
            Ok(())
        };
        for t in &info.tools {
            check_name(&t.name, "工具")?;
        }
        for c in &info.commands {
            check_name(&c.name, "命令")?;
        }
        for e in &info.events {
            check_name(&e.name, "事件")?;
        }

        let eager = matches!(info.load, LoadPolicy::Eager);
        let entry = Arc::new(PluginEntry {
            info,
            body,
            loaded: AtomicBool::new(false),
        });
        self.entries
            .write()
            .expect("registry poisoned")
            .insert(entry.info.namespace.clone(), entry.clone());
        if eager {
            self.load_plugin(&entry.info.namespace)?;
        }
        Ok(())
    }

    pub fn load_plugin(&self, namespace: &str) -> Result<(), PluginError> {
        let entry = {
            let entries = self.entries.read().expect("registry poisoned");
            entries
                .get(namespace)
                .cloned()
                .ok_or_else(|| PluginError::Internal(format!("未知插件：{namespace}")))?
        };
        if entry.loaded.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let targets = RegistrarTargets {
            handlers: &self.handlers,
            wire_to_full: &self.wire_to_full,
        };
        let registrar = EntryRegistrar::new(namespace, &entry.info, targets);
        let result = match &entry.body {
            PluginBody::User(register) => {
                let ctx = PluginContext {
                    handles: self.services.filter(&entry.info.requires),
                    registrar,
                };
                register(ctx)
            }
            PluginBody::Kernel(register) => {
                let ctx = KernelContext {
                    handles: self.services.clone(),
                    registrar,
                };
                register(ctx)
            }
        };
        if result.is_err() {
            entry.loaded.store(false, Ordering::SeqCst);
        }
        result
    }

    pub fn ensure_tool(&self, full: &str) -> Result<RegisteredEntry, PluginError> {
        self.ensure(full, EntryKind::Tool)
    }

    pub fn ensure_command(&self, full: &str) -> Result<RegisteredEntry, PluginError> {
        self.ensure(full, EntryKind::Command)
    }

    fn ensure(&self, full: &str, kind: EntryKind) -> Result<RegisteredEntry, PluginError> {
        if !self
            .handlers
            .read()
            .expect("registry poisoned")
            .contains_key(full)
        {
            let ns = full.split("::").next().unwrap_or_default().to_string();
            self.load_plugin(&ns)?;
        }
        let entry = self
            .handlers
            .read()
            .expect("registry poisoned")
            .get(full)
            .cloned();
        match entry {
            Some(e) if e.kind == kind => Ok(e),
            _ => Err(PluginError::Internal(format!("入口点不存在：{full}"))),
        }
    }

    pub fn resolve_wire(&self, wire: &str) -> Option<String> {
        {
            let map = self.wire_to_full.read().expect("registry poisoned");
            if let Some(full) = map.get(wire) {
                return Some(full.clone());
            }
        }
        let namespace = {
            let entries = self.entries.read().expect("registry poisoned");
            entries.values().find_map(|e| {
                let hit = e
                    .info
                    .tools
                    .iter()
                    .any(|t| full_to_wire(&full_name(&e.info.namespace, &t.name)) == wire);
                hit.then(|| e.info.namespace.clone())
            })
        }?;
        let _ = self.load_plugin(&namespace);
        self.wire_to_full
            .read()
            .expect("registry poisoned")
            .get(wire)
            .cloned()
    }

    pub fn entry_icon(&self, full_name: &str) -> Option<String> {
        self.handlers
            .read()
            .expect("registry poisoned")
            .get(full_name)
            .and_then(|e| e.icon.clone())
    }

    pub fn entry_title(&self, full: &str) -> Option<String> {
        let entries = self.entries.read().expect("registry poisoned");
        for e in entries.values() {
            let ns = &e.info.namespace;
            for t in &e.info.tools {
                if crate::contract::full_name(ns, &t.name) == full {
                    return Some(t.title.clone().unwrap_or_else(|| t.name.clone()));
                }
            }
            for c in &e.info.commands {
                if crate::contract::full_name(ns, &c.name) == full {
                    return Some(c.title.clone().unwrap_or_else(|| c.name.clone()));
                }
            }
        }
        None
    }

    pub fn model_tools(&self) -> Vec<ToolSchema> {
        let entries = self.entries.read().expect("registry poisoned");
        let mut out = Vec::new();
        for e in entries.values() {
            for t in &e.info.tools {
                if t.policy == CallerPolicy::UserAndModel {
                    out.push(ToolSchema {
                        name: full_to_wire(&full_name(&e.info.namespace, &t.name)),
                        description: t.description.clone(),
                        input_schema: serde_json::to_value(&t.params).unwrap_or_default(),
                    });
                }
            }
        }
        out
    }

    pub fn user_entries(&self) -> Vec<serde_json::Value> {
        let entries = self.entries.read().expect("registry poisoned");
        let mut out = Vec::new();
        for entry in entries.values() {
            let ns = &entry.info.namespace;
            for t in &entry.info.tools {
                if !t.user_visible {
                    continue;
                }
                out.push(serde_json::json!({
                    "entry": full_name(ns, &t.name),
                    "kind": "tool",
                    "title": t.title,
                    "group": t.group,
                    "policy": t.policy,
                    "description": t.description,
                    "icon": t.icon,
                    "params": t.params,
                }));
            }
            for c in &entry.info.commands {
                if !c.user_visible {
                    continue;
                }
                out.push(serde_json::json!({
                    "entry": full_name(ns, &c.name),
                    "kind": "command",
                    "title": c.title,
                    "group": c.group,
                    "policy": CallerPolicy::UserOnly,
                    "description": c.description,
                    "icon": c.icon,
                    "params": c.params,
                }));
            }
        }
        out
    }
}

pub fn tool_def(name: &str, description: &str, policy: CallerPolicy) -> ToolDef {
    ToolDef {
        name: name.into(),
        user_visible: true,
        title: None,
        group: None,
        description: description.into(),
        params: crate::contract::empty_params(),
        policy,
        timeout: None,
        icon: None,
    }
}
