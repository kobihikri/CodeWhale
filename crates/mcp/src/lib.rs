use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// MCP protocol revision advertised during the `initialize` handshake.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Configuration for a single MCP server process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique server identifier used for tool name qualification.
    pub name: String,
    /// Path or name of the server executable.
    pub command: String,
    /// Command-line arguments passed to the server process.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables set for the server process.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Whether this server should be started. Disabled servers are skipped.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Filter controlling which tools from an MCP server are exposed.
///
/// When `allow` is empty, all tools are permitted (unless denied).
/// `deny` takes precedence over `allow`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolFilter {
    /// Tool names to expose. Empty means expose all.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Tool names to exclude. Takes precedence over `allow`.
    #[serde(default)]
    pub deny: Vec<String>,
}

/// A complete MCP server definition including config and tool filter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerDefinition {
    /// Server process configuration.
    pub config: McpServerConfig,
    /// Tool filter controlling which tools are exposed.
    #[serde(default)]
    pub filter: ToolFilter,
}

/// Status of an individual MCP server during startup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpStartupStatus {
    /// Server process is starting.
    Starting,
    /// Server is ready to accept tool calls.
    Ready,
    /// Server failed to start.
    Failed { error: String },
    /// Server startup was cancelled (e.g., disabled in config).
    Cancelled,
}

/// Status update for a single MCP server during startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStartupUpdateEvent {
    /// Name of the server this update pertains to.
    pub server_name: String,
    /// Current startup status.
    pub status: McpStartupStatus,
}

/// Record of an MCP server that failed to start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStartupFailure {
    /// Name of the server that failed.
    pub server_name: String,
    /// Error message describing the failure.
    pub error: String,
}

/// Summary emitted after all MCP servers have completed startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStartupCompleteEvent {
    /// Names of servers that started successfully.
    pub ready: Vec<String>,
    /// Servers that failed with error details.
    pub failed: Vec<McpStartupFailure>,
    /// Names of servers that were skipped (disabled).
    pub cancelled: Vec<String>,
}

/// Describes a single tool provided by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDescriptor {
    /// Name of the server providing this tool.
    pub server_name: String,
    /// Original tool name as reported by the server.
    pub tool_name: String,
    /// Fully qualified name (e.g., `mcp__server__tool`).
    pub qualified_name: String,
    /// Human-readable description of what the tool does.
    pub description: Option<String>,
}

/// Describes a resource provided by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceDescriptor {
    /// Name of the server providing this resource.
    pub server_name: String,
    /// URI identifying the resource.
    pub uri: String,
    /// Human-readable description.
    pub description: Option<String>,
}

/// Trait abstracting an MCP client connection.
///
/// Implementations handle communication with a single MCP server process.
pub trait McpManagedClient: Send + Sync {
    /// List all tools provided by this server.
    fn list_tools(&self) -> Result<Vec<McpToolDescriptor>>;
    /// Invoke a tool by name with the given arguments.
    fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<Value>;
    /// List all resources provided by this server.
    fn list_resources(&self) -> Result<Vec<McpResourceDescriptor>>;
    /// Read a resource by URI.
    fn read_resource(&self, uri: &str) -> Result<Value>;
}

/// A simple in-memory MCP client for testing and default server stubs.
#[derive(Debug, Default)]
pub struct InMemoryMcpClient {
    tools: HashMap<String, Value>,
    resources: HashMap<String, Value>,
}

impl InMemoryMcpClient {
    /// Register a tool with a fixed response value.
    pub fn with_tool(mut self, name: &str, sample_result: Value) -> Self {
        self.tools.insert(name.to_string(), sample_result);
        self
    }

    /// Register a resource with a fixed data value.
    pub fn with_resource(mut self, uri: &str, data: Value) -> Self {
        self.resources.insert(uri.to_string(), data);
        self
    }
}

impl McpManagedClient for InMemoryMcpClient {
    fn list_tools(&self) -> Result<Vec<McpToolDescriptor>> {
        Ok(self
            .tools
            .keys()
            .map(|name| McpToolDescriptor {
                server_name: "in-memory".to_string(),
                tool_name: name.clone(),
                qualified_name: name.clone(),
                description: None,
            })
            .collect())
    }

    fn call_tool(&self, tool_name: &str, _arguments: Value) -> Result<Value> {
        self.tools
            .get(tool_name)
            .cloned()
            .with_context(|| format!("tool '{tool_name}' not found"))
    }

    fn list_resources(&self) -> Result<Vec<McpResourceDescriptor>> {
        Ok(self
            .resources
            .keys()
            .map(|uri| McpResourceDescriptor {
                server_name: "in-memory".to_string(),
                uri: uri.clone(),
                description: None,
            })
            .collect())
    }

    fn read_resource(&self, uri: &str) -> Result<Value> {
        self.resources
            .get(uri)
            .cloned()
            .with_context(|| format!("resource '{uri}' not found"))
    }
}

/// An MCP client backed by a real child process speaking JSON-RPC 2.0 over stdio.
///
/// This is the transport used by `codewhale mcp-server`: the configured
/// `command`/`args`/`env` are actually spawned and every `tools/*` and
/// `resources/*` request is forwarded to that process. Nothing here fabricates a
/// response — a server that cannot be spawned or that answers with a JSON-RPC
/// error surfaces as an error to the caller.
pub struct ChildProcessMcpClient {
    server_name: String,
    conn: Mutex<ChildRpcConnection>,
}

struct ChildRpcConnection {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl ChildProcessMcpClient {
    /// Spawn `config.command` and perform the MCP `initialize` handshake.
    pub fn spawn(config: &McpServerConfig) -> Result<Self> {
        if config.command.trim().is_empty() {
            bail!(
                "MCP server '{}' has no command configured; refusing to serve stub responses",
                config.name
            );
        }
        let mut child = Command::new(&config.command)
            .args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| {
                format!(
                    "failed to spawn MCP server '{}' ({})",
                    config.name, config.command
                )
            })?;
        let stdin = child
            .stdin
            .take()
            .context("spawned MCP server has no stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("spawned MCP server has no stdout")?;

        let client = Self {
            server_name: config.name.clone(),
            conn: Mutex::new(ChildRpcConnection {
                child,
                stdin,
                stdout: BufReader::new(stdout),
                next_id: 1,
            }),
        };
        client.handshake()?;
        Ok(client)
    }

    fn handshake(&self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "codewhale", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        self.notify("notifications/initialized", json!({}))
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        let mut conn = self.lock()?;
        let payload = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        writeln!(conn.stdin, "{payload}")
            .and_then(|()| conn.stdin.flush())
            .with_context(|| {
                format!(
                    "failed to send '{method}' to MCP server '{}'",
                    self.server_name
                )
            })
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let mut conn = self.lock()?;
        let id = conn.next_id;
        conn.next_id += 1;
        let payload = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(conn.stdin, "{payload}")
            .and_then(|()| conn.stdin.flush())
            .with_context(|| {
                format!(
                    "failed to send '{method}' to MCP server '{}'",
                    self.server_name
                )
            })?;

        loop {
            let mut line = String::new();
            let read = conn.stdout.read_line(&mut line).with_context(|| {
                format!("failed to read from MCP server '{}'", self.server_name)
            })?;
            if read == 0 {
                bail!(
                    "MCP server '{}' closed its stdout while awaiting '{method}'",
                    self.server_name
                );
            }
            let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            // Ignore notifications and responses to other in-flight requests.
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(err) = message.get("error") {
                bail!(
                    "MCP server '{}' returned an error for '{method}': {err}",
                    self.server_name
                );
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ChildRpcConnection>> {
        self.conn.lock().map_err(|_| {
            anyhow::anyhow!("MCP server '{}' connection is poisoned", self.server_name)
        })
    }
}

impl Drop for ChildProcessMcpClient {
    fn drop(&mut self) {
        if let Ok(mut conn) = self.conn.lock() {
            let _ = conn.child.kill();
            let _ = conn.child.wait();
        }
    }
}

impl McpManagedClient for ChildProcessMcpClient {
    fn list_tools(&self) -> Result<Vec<McpToolDescriptor>> {
        let result = self.request("tools/list", json!({}))?;
        let entries = result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(entries
            .iter()
            .filter_map(|entry| {
                let tool_name = entry.get("name").and_then(Value::as_str)?.to_string();
                Some(McpToolDescriptor {
                    qualified_name: qualify_tool_name(&self.server_name, &tool_name),
                    server_name: self.server_name.clone(),
                    tool_name,
                    description: entry
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            })
            .collect())
    }

    fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<Value> {
        self.request(
            "tools/call",
            json!({ "name": tool_name, "arguments": arguments }),
        )
    }

    fn list_resources(&self) -> Result<Vec<McpResourceDescriptor>> {
        let result = self.request("resources/list", json!({}))?;
        let entries = result
            .get("resources")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(entries
            .iter()
            .filter_map(|entry| {
                Some(McpResourceDescriptor {
                    server_name: self.server_name.clone(),
                    uri: entry.get("uri").and_then(Value::as_str)?.to_string(),
                    description: entry
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            })
            .collect())
    }

    fn read_resource(&self, uri: &str) -> Result<Value> {
        self.request("resources/read", json!({ "uri": uri }))
    }
}

/// Manages multiple MCP server connections and their tool/resource registrations.
#[derive(Default)]
pub struct McpManager {
    configs: HashMap<String, (McpServerConfig, ToolFilter)>,
    clients: HashMap<String, Box<dyn McpManagedClient>>,
}

impl McpManager {
    /// Register an MCP server with its config, tool filter, and client implementation.
    ///
    /// Fails when the server's name sanitizes to the same qualified-tool prefix as an
    /// already-registered server with a *different* name (e.g. `my-server` vs `my_server`).
    /// Allowing both would make `mcp__my_server__*` ambiguous, so one server could shadow
    /// or intercept tool calls meant for the other.
    pub fn register_server(
        &mut self,
        config: McpServerConfig,
        filter: ToolFilter,
        client: Box<dyn McpManagedClient>,
    ) -> Result<()> {
        let sanitized = sanitize_component(&config.name);
        if let Some(existing) = self
            .configs
            .keys()
            .find(|name| **name != config.name && sanitize_component(name) == sanitized)
        {
            bail!(
                "MCP server '{}' collides with already-registered server '{existing}': both qualify tools as 'mcp__{sanitized}__*'",
                config.name
            );
        }
        self.clients.insert(config.name.clone(), client);
        self.configs.insert(config.name.clone(), (config, filter));
        Ok(())
    }

    /// Start all registered servers, emitting status updates via the callback.
    ///
    /// Returns a summary of which servers are ready, failed, or cancelled.
    pub fn start_all<F>(&self, mut emit: F) -> McpStartupCompleteEvent
    where
        F: FnMut(McpStartupUpdateEvent),
    {
        let mut ready = Vec::new();
        let mut failed = Vec::new();
        let mut cancelled = Vec::new();
        for (server_name, (cfg, _)) in &self.configs {
            if !cfg.enabled {
                emit(McpStartupUpdateEvent {
                    server_name: server_name.clone(),
                    status: McpStartupStatus::Cancelled,
                });
                cancelled.push(server_name.clone());
                continue;
            }
            emit(McpStartupUpdateEvent {
                server_name: server_name.clone(),
                status: McpStartupStatus::Starting,
            });
            if self.clients.contains_key(server_name) {
                emit(McpStartupUpdateEvent {
                    server_name: server_name.clone(),
                    status: McpStartupStatus::Ready,
                });
                ready.push(server_name.clone());
            } else {
                let error = "client not registered".to_string();
                emit(McpStartupUpdateEvent {
                    server_name: server_name.clone(),
                    status: McpStartupStatus::Failed {
                        error: error.clone(),
                    },
                });
                failed.push(McpStartupFailure {
                    server_name: server_name.clone(),
                    error,
                });
            }
        }
        McpStartupCompleteEvent {
            ready,
            failed,
            cancelled,
        }
    }

    /// Stop a running server by removing its client.
    pub fn stop_server(&mut self, server_name: &str) -> Result<()> {
        self.clients
            .remove(server_name)
            .with_context(|| format!("server '{server_name}' is not running"))?;
        Ok(())
    }

    /// Remove a server entirely (config and client).
    pub fn unregister_server(&mut self, server_name: &str) -> Result<()> {
        let had_config = self.configs.remove(server_name).is_some();
        self.clients.remove(server_name);
        if !had_config {
            bail!("server '{server_name}' is not registered");
        }
        Ok(())
    }

    /// List all tools from all running servers, applying tool filters.
    pub fn list_tools(&self) -> Result<Vec<McpToolDescriptor>> {
        let mut out = Vec::new();
        for (server_name, (_, filter)) in &self.configs {
            let Some(client) = self.clients.get(server_name) else {
                continue;
            };
            let tools = client.list_tools()?;
            for tool in tools {
                if !allowed_by_filter(&tool.tool_name, filter) {
                    continue;
                }
                let qualified_name = qualify_tool_name(server_name, &tool.tool_name);
                out.push(McpToolDescriptor {
                    server_name: server_name.clone(),
                    tool_name: tool.tool_name,
                    qualified_name,
                    description: tool.description,
                });
            }
        }
        Ok(out)
    }

    /// Call a tool on a specific server by name.
    pub fn call_tool(&self, server_name: &str, tool_name: &str, arguments: Value) -> Result<Value> {
        let client = self
            .clients
            .get(server_name)
            .with_context(|| format!("MCP server '{server_name}' not available"))?;
        client.call_tool(tool_name, arguments)
    }

    /// Call a tool using its fully qualified name (e.g., `mcp__server__tool`).
    ///
    /// Resolution is a pure lookup that never invokes a tool, so the tool is called
    /// exactly once. A failing call is propagated as-is — it is never retried against
    /// another server, because retrying would re-run any side effect the first attempt
    /// already performed.
    pub fn call_qualified_tool(
        &self,
        qualified_tool_name: &str,
        arguments: Value,
    ) -> Result<Value> {
        let (server_name, tool_name) = self.resolve_qualified_tool(qualified_tool_name)?;
        self.call_tool(&server_name, &tool_name, arguments)
    }

    /// Resolve a qualified tool name to its `(server, tool)` pair without calling anything.
    ///
    /// Resolution prefers the server whose sanitized name matches the qualified name's
    /// server segment, then falls back to a scan in sorted server order so the result never
    /// depends on `HashMap` iteration order.
    fn resolve_qualified_tool(&self, qualified_tool_name: &str) -> Result<(String, String)> {
        let parsed = parse_qualified_tool_name(qualified_tool_name)
            .with_context(|| format!("invalid qualified MCP tool name: {qualified_tool_name}"));

        let exact: Vec<&String> = match &parsed {
            Ok((server_segment, _)) => self
                .configs
                .keys()
                .filter(|name| sanitize_component(name) == *server_segment)
                .collect(),
            Err(_) => Vec::new(),
        };
        if exact.len() > 1 {
            let mut names: Vec<&str> = exact.iter().map(|name| name.as_str()).collect();
            names.sort_unstable();
            bail!(
                "qualified MCP tool name '{qualified_tool_name}' is ambiguous across servers: {}",
                names.join(", ")
            );
        }

        let mut candidates: Vec<&String> = if exact.len() == 1 {
            exact
        } else {
            self.configs.keys().collect()
        };
        candidates.sort_unstable();

        for server_name in candidates {
            let Some(client) = self.clients.get(server_name) else {
                continue;
            };
            let Some((_, filter)) = self.configs.get(server_name) else {
                continue;
            };
            let Ok(tools) = client.list_tools() else {
                continue;
            };
            for tool in tools {
                if !allowed_by_filter(&tool.tool_name, filter) {
                    continue;
                }
                if qualify_tool_name(server_name, &tool.tool_name) == qualified_tool_name {
                    return Ok((server_name.clone(), tool.tool_name));
                }
            }
        }

        // Nothing advertised this qualified name; fall back to the literal segments so a
        // server that cannot list its tools is still reachable by name.
        let (server_segment, tool_segment) = parsed?;
        let server_name = self
            .configs
            .keys()
            .find(|name| sanitize_component(name) == server_segment)
            .cloned()
            .unwrap_or(server_segment);
        Ok((server_name, tool_segment))
    }

    /// List all resources from all running servers.
    pub fn list_resources(&self) -> Result<Vec<McpResourceDescriptor>> {
        let mut out = Vec::new();
        for server_name in self.configs.keys() {
            let Some(client) = self.clients.get(server_name) else {
                continue;
            };
            for mut resource in client.list_resources()? {
                resource.server_name = server_name.clone();
                out.push(resource);
            }
        }
        Ok(out)
    }

    /// Read a resource from a specific server.
    pub fn read_resource(&self, server_name: &str, uri: &str) -> Result<Value> {
        let client = self
            .clients
            .get(server_name)
            .with_context(|| format!("MCP server '{server_name}' not available"))?;
        client.read_resource(uri)
    }

    /// Generate sandbox state update notices for all registered servers.
    pub fn update_sandbox_state(&self, sandbox_mode: &str, cwd: &str) -> Result<Vec<Value>> {
        let mut notices = Vec::new();
        for server_name in self.configs.keys() {
            notices.push(json!({
                "server_name": server_name,
                "method": "codex/sandbox-state/update",
                "params": {
                    "sandbox_mode": sandbox_mode,
                    "cwd": cwd
                }
            }));
        }
        Ok(notices)
    }
}

fn default_true() -> bool {
    true
}

fn allowed_by_filter(name: &str, filter: &ToolFilter) -> bool {
    if filter.deny.iter().any(|pattern| pattern == name) {
        return false;
    }
    if filter.allow.is_empty() {
        return true;
    }
    filter.allow.iter().any(|pattern| pattern == name)
}

fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn qualify_tool_name(server: &str, tool: &str) -> String {
    let server = sanitize_component(server);
    let tool = sanitize_component(tool);
    let mut name = format!("mcp__{server}__{tool}");
    if name.len() > 64 {
        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        let hash = format!("{:x}", hasher.finish());
        let suffix = format!("_{}", &hash[..12]);
        let component_budget = 64 - "mcp__".len() - "__".len() - suffix.len();
        let mut server_len = server.len().min(component_budget / 2);
        let mut tool_len = tool.len().min(component_budget - server_len);
        let remaining = component_budget - server_len - tool_len;
        if remaining > 0 {
            let server_extra = (server.len() - server_len).min(remaining);
            server_len += server_extra;
            tool_len += (tool.len() - tool_len).min(remaining - server_extra);
        }
        name = format!(
            "mcp__{}__{}{}",
            &server[..server_len],
            &tool[..tool_len],
            suffix
        );
    }
    name
}

fn parse_qualified_tool_name(value: &str) -> Result<(String, String)> {
    let Some(stripped) = value.strip_prefix("mcp__") else {
        bail!("missing mcp__ prefix");
    };
    let mut split = stripped.splitn(2, "__");
    let server = split
        .next()
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .context("missing server segment")?;
    let tool = split
        .next()
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .context("missing tool segment")?;
    Ok((server, tool))
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug)]
struct JsonRpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ToolsListParams {
    #[serde(default)]
    server: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ToolsCallParams {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Deserialize)]
struct ResourcesListParams {
    #[serde(default)]
    server: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResourcesReadParams {
    #[serde(default)]
    server: Option<String>,
    uri: String,
}

#[derive(Debug, Deserialize)]
struct ServerRegisterParams {
    server: McpServerConfig,
    #[serde(default)]
    filter: ToolFilter,
    #[serde(default = "default_true")]
    start: bool,
}

#[derive(Debug, Deserialize)]
struct ServerNameParams {
    name: String,
}

struct StdioMcpState {
    manager: McpManager,
    definitions: HashMap<String, McpServerDefinition>,
    running: HashMap<String, bool>,
    lifecycle_state: String,
}

/// Run an MCP stdio server that reads JSON-RPC requests from stdin and writes responses to stdout.
///
/// Returns the final server definitions after the session ends (useful for persisting
/// runtime changes like server registrations).
pub fn run_stdio_server(
    initial_definitions: Vec<McpServerDefinition>,
) -> Result<Vec<McpServerDefinition>> {
    use std::io;

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    let mut state = build_stdio_state(initial_definitions);

    for line in stdin.lock().lines() {
        let line = line.context("failed to read stdio line")?;
        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                let msg = jsonrpc_error(
                    None,
                    JsonRpcError::parse_error(format!("invalid json: {err}")),
                );
                writeln!(stdout, "{msg}")?;
                stdout.flush()?;
                continue;
            }
        };

        if request
            .jsonrpc
            .as_deref()
            .is_some_and(|version| version != "2.0")
        {
            if should_respond_to_jsonrpc(&request.id) {
                let response = jsonrpc_error(
                    request.id,
                    JsonRpcError::invalid_request("jsonrpc version must be 2.0"),
                );
                writeln!(stdout, "{response}")?;
                stdout.flush()?;
            }
            continue;
        }

        if !should_respond_to_jsonrpc(&request.id) {
            match dispatch_stdio_request(&mut state, &request.method, request.params) {
                Ok((_, should_exit)) if should_exit => break,
                Ok(_) | Err(_) => {}
            }
            continue;
        }

        let response = match dispatch_stdio_request(&mut state, &request.method, request.params) {
            Ok((result, should_exit)) => {
                let payload = jsonrpc_result(request.id, result);
                writeln!(stdout, "{payload}")?;
                stdout.flush()?;
                if should_exit {
                    break;
                }
                continue;
            }
            Err(err) => jsonrpc_error(request.id, err),
        };

        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }

    state.lifecycle_state = "stopped".to_string();
    let _ = writeln!(stderr, "deepseek-mcp stdio server exited");
    let mut definitions: Vec<McpServerDefinition> = state.definitions.into_values().collect();
    definitions.sort_by(|a, b| a.config.name.cmp(&b.config.name));
    Ok(definitions)
}

fn build_stdio_state(initial_definitions: Vec<McpServerDefinition>) -> StdioMcpState {
    let mut manager = McpManager::default();
    let mut definitions = HashMap::new();
    let mut running = HashMap::new();

    for definition in initial_definitions {
        let name = definition.config.name.clone();
        let should_start = definition.config.enabled;
        definitions.insert(name.clone(), definition.clone());
        if should_start {
            // A spawn failure or name collision means this server cannot serve real
            // traffic; keep it defined but not running rather than aliasing it or
            // answering on its behalf with fabricated results.
            let registered = spawn_stdio_client(&definition.config).and_then(|client| {
                manager.register_server(
                    definition.config.clone(),
                    definition.filter.clone(),
                    client,
                )
            });
            if let Err(err) = &registered {
                let _ = writeln!(std::io::stderr(), "mcp: server '{name}' not started: {err}");
            }
            running.insert(name, registered.is_ok());
        } else {
            running.insert(name, false);
        }
    }

    StdioMcpState {
        manager,
        definitions,
        running,
        lifecycle_state: "running".to_string(),
    }
}

/// Build the client used to talk to a configured MCP server.
///
/// Always a real spawned child process: there is no stub fallback, so a
/// misconfigured server fails loudly instead of answering with fabricated
/// "ok" results.
fn spawn_stdio_client(config: &McpServerConfig) -> Result<Box<dyn McpManagedClient>> {
    Ok(Box::new(ChildProcessMcpClient::spawn(config)?))
}

fn default_rpc_methods() -> Vec<&'static str> {
    vec![
        "initialize",
        "healthz",
        "capabilities",
        "tools/list",
        "tools/call",
        "resources/list",
        "resources/read",
        "server/list",
        "server/register",
        "server/start",
        "server/stop",
        "server/unregister",
        "shutdown",
    ]
}

fn lifecycle_snapshot(state: &StdioMcpState) -> Value {
    let mut servers: Vec<Value> = state
        .definitions
        .iter()
        .map(|(name, definition)| {
            let is_running = state.running.get(name).copied().unwrap_or(false);
            json!({
                "name": name,
                "enabled": definition.config.enabled,
                "running": is_running,
                "command": definition.config.command.clone(),
                "args": definition.config.args.clone(),
            })
        })
        .collect();
    servers.sort_by(|a, b| {
        let a_name = a.get("name").and_then(Value::as_str).unwrap_or_default();
        let b_name = b.get("name").and_then(Value::as_str).unwrap_or_default();
        a_name.cmp(b_name)
    });

    let running_count = state.running.values().filter(|running| **running).count();
    json!({
        "status": state.lifecycle_state,
        "servers": servers,
        "counts": {
            "defined": state.definitions.len(),
            "running": running_count
        }
    })
}

fn params_or_object(params: Value) -> Value {
    if params.is_null() { json!({}) } else { params }
}

fn parse_params<T: DeserializeOwned>(params: Value) -> std::result::Result<T, JsonRpcError> {
    serde_json::from_value(params).map_err(|err| JsonRpcError::invalid_params(err.to_string()))
}

fn parse_server_from_uri(uri: &str) -> Option<String> {
    let stripped = uri.strip_prefix("mcp://")?;
    let server = stripped.split('/').next()?;
    if server.is_empty() {
        None
    } else {
        Some(server.to_string())
    }
}

fn dispatch_stdio_request(
    state: &mut StdioMcpState,
    method: &str,
    params: Value,
) -> std::result::Result<(Value, bool), JsonRpcError> {
    match method {
        "initialize" | "capabilities" => Ok((
            json!({
                "server": "deepseek-mcp",
                "transport": "stdio",
                "methods": default_rpc_methods(),
                "lifecycle": lifecycle_snapshot(state)
            }),
            false,
        )),
        "healthz" => Ok((
            json!({
                "status": "ok",
                "service": "deepseek-mcp",
                "transport": "stdio",
                "lifecycle": lifecycle_snapshot(state)
            }),
            false,
        )),
        "tools/list" => {
            let parsed: ToolsListParams = parse_params(params_or_object(params))?;
            let mut tools = state
                .manager
                .list_tools()
                .map_err(|err| JsonRpcError::internal(err.to_string()))?;
            if let Some(server) = parsed.server {
                tools.retain(|tool| tool.server_name == server);
            }
            Ok((json!({ "tools": tools }), false))
        }
        "tools/call" => {
            let parsed: ToolsCallParams = parse_params(params_or_object(params))?;
            let ToolsCallParams {
                name,
                tool,
                server,
                arguments,
            } = parsed;
            let tool_name = name
                .or(tool)
                .context("missing tool name")
                .map_err(|err| JsonRpcError::invalid_params(err.to_string()))?;
            let arguments = if arguments.is_null() {
                json!({})
            } else {
                arguments
            };
            let result = if tool_name.starts_with("mcp__") {
                state
                    .manager
                    .call_qualified_tool(&tool_name, arguments)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?
            } else {
                let server = server
                    .context("missing server for unqualified tool")
                    .map_err(|err| JsonRpcError::invalid_params(err.to_string()))?;
                state
                    .manager
                    .call_tool(&server, &tool_name, arguments)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?
            };
            Ok((json!({ "result": result }), false))
        }
        "resources/list" => {
            let parsed: ResourcesListParams = parse_params(params_or_object(params))?;
            let mut resources = state
                .manager
                .list_resources()
                .map_err(|err| JsonRpcError::internal(err.to_string()))?;
            if let Some(server) = parsed.server {
                resources.retain(|resource| resource.server_name == server);
            }
            Ok((json!({ "resources": resources }), false))
        }
        "resources/read" => {
            let parsed: ResourcesReadParams = parse_params(params_or_object(params))?;
            let ResourcesReadParams { server, uri } = parsed;
            let server_name = server
                .or_else(|| parse_server_from_uri(&uri))
                .context("missing server for resource read")
                .map_err(|err| JsonRpcError::invalid_params(err.to_string()))?;
            let value = state
                .manager
                .read_resource(&server_name, &uri)
                .map_err(|err| JsonRpcError::internal(err.to_string()))?;
            Ok((json!({ "resource": value }), false))
        }
        "server/list" | "servers/list" => {
            Ok((json!({ "lifecycle": lifecycle_snapshot(state) }), false))
        }
        "server/register" | "servers/register" => {
            let parsed: ServerRegisterParams = parse_params(params_or_object(params))?;
            let name = parsed.server.name.clone();
            if name.trim().is_empty() {
                return Err(JsonRpcError::invalid_params(
                    "server.name must not be empty",
                ));
            }

            if state.definitions.contains_key(&name) {
                let _ = state.manager.unregister_server(&name);
            }
            state.definitions.insert(
                name.clone(),
                McpServerDefinition {
                    config: parsed.server.clone(),
                    filter: parsed.filter.clone(),
                },
            );
            let should_run = parsed.start && parsed.server.enabled;
            if should_run {
                state
                    .manager
                    .register_server(
                        parsed.server.clone(),
                        parsed.filter.clone(),
                        spawn_stdio_client(&parsed.server)
                            .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                    )
                    .map_err(|err| JsonRpcError::invalid_params(err.to_string()))?;
            }
            state.running.insert(name, should_run);
            Ok((json!({ "lifecycle": lifecycle_snapshot(state) }), false))
        }
        "server/start" | "servers/start" => {
            let parsed: ServerNameParams = parse_params(params_or_object(params))?;
            let definition = state
                .definitions
                .get(&parsed.name)
                .cloned()
                .with_context(|| format!("server '{}' is not defined", parsed.name))
                .map_err(|err| JsonRpcError::invalid_params(err.to_string()))?;
            if !definition.config.enabled {
                return Err(JsonRpcError::invalid_params(format!(
                    "server '{}' is disabled",
                    parsed.name
                )));
            }
            if !state.running.get(&parsed.name).copied().unwrap_or(false) {
                state
                    .manager
                    .register_server(
                        definition.config.clone(),
                        definition.filter.clone(),
                        spawn_stdio_client(&definition.config)
                            .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                    )
                    .map_err(|err| JsonRpcError::invalid_params(err.to_string()))?;
                state.running.insert(parsed.name, true);
            }
            Ok((json!({ "lifecycle": lifecycle_snapshot(state) }), false))
        }
        "server/stop" | "servers/stop" => {
            let parsed: ServerNameParams = parse_params(params_or_object(params))?;
            if state.running.get(&parsed.name).copied().unwrap_or(false) {
                state
                    .manager
                    .stop_server(&parsed.name)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?;
            }
            state.running.insert(parsed.name, false);
            Ok((json!({ "lifecycle": lifecycle_snapshot(state) }), false))
        }
        "server/unregister" | "servers/unregister" => {
            let parsed: ServerNameParams = parse_params(params_or_object(params))?;
            if state.definitions.remove(&parsed.name).is_none() {
                return Err(JsonRpcError::invalid_params(format!(
                    "server '{}' is not defined",
                    parsed.name
                )));
            }
            let _ = state.manager.unregister_server(&parsed.name);
            state.running.remove(&parsed.name);
            Ok((json!({ "lifecycle": lifecycle_snapshot(state) }), false))
        }
        "shutdown" => {
            state.lifecycle_state = "shutting_down".to_string();
            Ok((
                json!({
                    "ok": true,
                    "lifecycle": lifecycle_snapshot(state)
                }),
                true,
            ))
        }
        _ => Err(JsonRpcError::method_not_found(method)),
    }
}

fn jsonrpc_result(id: Option<Value>, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result
    })
}

fn should_respond_to_jsonrpc(id: &Option<Value>) -> bool {
    id.is_some()
}

fn jsonrpc_error(id: Option<Value>, err: JsonRpcError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {
            "code": err.code,
            "message": err.message,
            "data": err.data
        }
    })
}

impl JsonRpcError {
    fn parse_error(message: impl Into<String>) -> Self {
        Self {
            code: -32700,
            message: message.into(),
            data: None,
        }
    }

    fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: message.into(),
            data: None,
        }
    }

    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("unsupported method: {method}"),
            data: None,
        }
    }

    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoMcpClient;

    impl McpManagedClient for EchoMcpClient {
        fn list_tools(&self) -> Result<Vec<McpToolDescriptor>> {
            Ok(vec![])
        }

        fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<Value> {
            if tool_name == "error" {
                bail!("intentional error for testing");
            }
            Ok(arguments)
        }

        fn list_resources(&self) -> Result<Vec<McpResourceDescriptor>> {
            Ok(vec![])
        }

        fn read_resource(&self, _uri: &str) -> Result<Value> {
            bail!("not supported")
        }
    }

    // ── InMemoryMcpClient ──────────────────────────────────────────────

    #[test]
    fn in_memory_client_list_tools_returns_registered() {
        let client = InMemoryMcpClient::default()
            .with_tool("echo", json!({"output": "hi"}))
            .with_tool("greet", json!({"msg": "hello"}));
        let tools = client.list_tools().unwrap();
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools.iter().map(|t| t.tool_name.as_str()).collect();
        assert!(names.contains(&"echo"));
        assert!(names.contains(&"greet"));
    }

    #[test]
    fn in_memory_client_call_tool_returns_value() {
        let client = InMemoryMcpClient::default().with_tool("echo", json!({"output": "hi"}));
        let result = client.call_tool("echo", json!({})).unwrap();
        assert_eq!(result["output"], "hi");
    }

    #[test]
    fn in_memory_client_call_tool_errors_on_missing() {
        let client = InMemoryMcpClient::default();
        let err = client.call_tool("nope", json!({})).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn in_memory_client_list_resources_returns_registered() {
        let client = InMemoryMcpClient::default()
            .with_resource("mcp://s/health", json!({"ok": true}))
            .with_resource("mcp://s/caps", json!({"tools": []}));
        let resources = client.list_resources().unwrap();
        assert_eq!(resources.len(), 2);
    }

    #[test]
    fn in_memory_client_read_resource_returns_value() {
        let client =
            InMemoryMcpClient::default().with_resource("mcp://s/health", json!({"ok": true}));
        let result = client.read_resource("mcp://s/health").unwrap();
        assert_eq!(result["ok"], true);
    }

    #[test]
    fn in_memory_client_read_resource_errors_on_missing() {
        let client = InMemoryMcpClient::default();
        let err = client.read_resource("mcp://s/nope").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    // ── McpManager ─────────────────────────────────────────────────────

    fn make_server_config(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            command: "test".to_string(),
            args: vec![],
            env: HashMap::new(),
            enabled: true,
        }
    }

    #[test]
    fn manager_start_all_marks_ready_for_registered_clients() {
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config("s1"),
                ToolFilter::default(),
                Box::new(InMemoryMcpClient::default().with_tool("t", json!(null))),
            )
            .unwrap();
        let mut events = Vec::new();
        let summary = manager.start_all(|e| events.push(e));
        assert_eq!(summary.ready, vec!["s1"]);
        assert!(summary.failed.is_empty());
        assert!(events.iter().any(|event| {
            event.server_name == "s1" && event.status == McpStartupStatus::Starting
        }));
        assert!(
            events.iter().any(|event| {
                event.server_name == "s1" && event.status == McpStartupStatus::Ready
            })
        );
    }

    #[test]
    fn manager_start_all_marks_failed_when_client_missing() {
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config("s1"),
                ToolFilter::default(),
                Box::new(InMemoryMcpClient::default()),
            )
            .unwrap();
        manager.stop_server("s1").unwrap();
        let summary = manager.start_all(|_| {});
        assert!(summary.ready.is_empty());
        assert_eq!(summary.failed.len(), 1);
        assert_eq!(summary.failed[0].server_name, "s1");
    }

    #[test]
    fn manager_start_all_cancels_disabled_servers() {
        let mut manager = McpManager::default();
        let mut cfg = make_server_config("s1");
        cfg.enabled = false;
        manager
            .register_server(
                cfg,
                ToolFilter::default(),
                Box::new(InMemoryMcpClient::default()),
            )
            .unwrap();
        let summary = manager.start_all(|_| {});
        assert!(summary.ready.is_empty());
        assert_eq!(summary.cancelled, vec!["s1"]);
    }

    #[test]
    fn manager_list_tools_applies_filter() {
        let mut manager = McpManager::default();
        let client = InMemoryMcpClient::default()
            .with_tool("allowed", json!(null))
            .with_tool("denied", json!(null));
        manager
            .register_server(
                make_server_config("s1"),
                ToolFilter {
                    allow: vec!["allowed".to_string()],
                    deny: vec![],
                },
                Box::new(client),
            )
            .unwrap();
        let tools = manager.list_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_name, "allowed");
    }

    #[test]
    fn manager_list_tools_deny_overrides_allow() {
        let mut manager = McpManager::default();
        let client = InMemoryMcpClient::default()
            .with_tool("a", json!(null))
            .with_tool("b", json!(null));
        manager
            .register_server(
                make_server_config("s1"),
                ToolFilter {
                    allow: vec!["a".to_string(), "b".to_string()],
                    deny: vec!["b".to_string()],
                },
                Box::new(client),
            )
            .unwrap();
        let tools = manager.list_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_name, "a");
    }

    #[test]
    fn manager_call_tool_delegates_to_client() {
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config("s1"),
                ToolFilter::default(),
                Box::new(InMemoryMcpClient::default().with_tool("t", json!({"v": 42}))),
            )
            .unwrap();
        let result = manager.call_tool("s1", "t", json!({})).unwrap();
        assert_eq!(result["v"], 42);
    }

    #[test]
    fn manager_call_tool_passes_arguments_to_client() {
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config("s1"),
                ToolFilter::default(),
                Box::new(EchoMcpClient),
            )
            .unwrap();
        let args = json!({"hello": "world", "num": 100});
        let result = manager.call_tool("s1", "echo", args.clone()).unwrap();
        assert_eq!(result, args);
    }

    #[test]
    fn manager_call_tool_propagates_client_error() {
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config("s1"),
                ToolFilter::default(),
                Box::new(EchoMcpClient),
            )
            .unwrap();
        let err = manager.call_tool("s1", "error", json!({})).unwrap_err();
        assert!(err.to_string().contains("intentional error for testing"));
    }

    #[test]
    fn manager_call_tool_errors_on_missing_server() {
        let manager = McpManager::default();
        let err = manager.call_tool("nope", "t", json!({})).unwrap_err();
        assert!(err.to_string().contains("not available"));
    }

    #[test]
    fn manager_call_qualified_tool_parses_name() {
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config("my_server"),
                ToolFilter::default(),
                Box::new(InMemoryMcpClient::default().with_tool("my_tool", json!({"ok": true}))),
            )
            .unwrap();
        let result = manager
            .call_qualified_tool("mcp__my_server__my_tool", json!({}))
            .unwrap();
        assert_eq!(result["ok"], true);
    }

    #[test]
    fn manager_call_qualified_tool_handles_truncated_names() {
        let long_server = "server".repeat(20);
        let long_tool = "tool".repeat(20);
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config(&long_server),
                ToolFilter::default(),
                Box::new(InMemoryMcpClient::default().with_tool(&long_tool, json!({"ok": true}))),
            )
            .unwrap();
        let tools = manager.list_tools().unwrap();
        let qualified = &tools[0].qualified_name;
        assert!(qualified.len() <= 64);
        assert!(parse_qualified_tool_name(qualified).is_ok());

        let result = manager.call_qualified_tool(qualified, json!({})).unwrap();
        assert_eq!(result["ok"], true);
    }

    /// Client that records how many times `call_tool` ran and always fails.
    struct CountingFailClient {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tool: String,
    }

    impl McpManagedClient for CountingFailClient {
        fn list_tools(&self) -> Result<Vec<McpToolDescriptor>> {
            Ok(vec![McpToolDescriptor {
                server_name: "counting".to_string(),
                tool_name: self.tool.clone(),
                qualified_name: self.tool.clone(),
                description: None,
            }])
        }

        fn call_tool(&self, _tool_name: &str, _arguments: Value) -> Result<Value> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            bail!("boom")
        }

        fn list_resources(&self) -> Result<Vec<McpResourceDescriptor>> {
            Ok(vec![])
        }

        fn read_resource(&self, _uri: &str) -> Result<Value> {
            bail!("not supported")
        }
    }

    #[test]
    fn manager_register_server_rejects_sanitized_name_collision() {
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config("my-server"),
                ToolFilter::default(),
                Box::new(InMemoryMcpClient::default()),
            )
            .unwrap();
        // `my-server` and `my_server` both sanitize to `my_server`, so both would
        // claim the `mcp__my_server__*` prefix.
        let err = manager
            .register_server(
                make_server_config("my_server"),
                ToolFilter::default(),
                Box::new(InMemoryMcpClient::default()),
            )
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("my_server"), "unexpected error: {msg}");
        assert!(msg.contains("my-server"), "unexpected error: {msg}");
        // The colliding server must not have been registered.
        assert_eq!(manager.configs.len(), 1);
        assert!(manager.configs.contains_key("my-server"));
    }

    #[test]
    fn manager_register_server_allows_reregistering_same_name() {
        let mut manager = McpManager::default();
        for _ in 0..2 {
            manager
                .register_server(
                    make_server_config("s1"),
                    ToolFilter::default(),
                    Box::new(InMemoryMcpClient::default().with_tool("t", json!({"ok": true}))),
                )
                .unwrap();
        }
        assert_eq!(manager.configs.len(), 1);
    }

    #[test]
    fn manager_call_qualified_tool_invokes_failing_tool_exactly_once() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config("s1"),
                ToolFilter::default(),
                Box::new(CountingFailClient {
                    calls: std::sync::Arc::clone(&calls),
                    tool: "risky".to_string(),
                }),
            )
            .unwrap();

        let err = manager
            .call_qualified_tool("mcp__s1__risky", json!({}))
            .unwrap_err();
        assert!(err.to_string().contains("boom"));
        // The failure must propagate as-is rather than being retried, otherwise any
        // side effect the tool performed would run twice.
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn manager_call_qualified_tool_resolves_to_the_named_server() {
        // Both servers expose a tool called `ping`; only the qualified prefix
        // distinguishes them, and resolution must not depend on HashMap order.
        let mut manager = McpManager::default();
        for name in ["alpha", "beta"] {
            manager
                .register_server(
                    make_server_config(name),
                    ToolFilter::default(),
                    Box::new(InMemoryMcpClient::default().with_tool("ping", json!({"from": name}))),
                )
                .unwrap();
        }

        for _ in 0..20 {
            let a = manager
                .call_qualified_tool("mcp__alpha__ping", json!({}))
                .unwrap();
            assert_eq!(a["from"], "alpha");
            let b = manager
                .call_qualified_tool("mcp__beta__ping", json!({}))
                .unwrap();
            assert_eq!(b["from"], "beta");
        }
    }

    #[test]
    fn manager_call_qualified_tool_does_not_fall_through_to_a_filtered_server() {
        // `alpha` denies `ping`, so `mcp__alpha__ping` must not silently resolve
        // to `beta`'s identically-named tool.
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config("alpha"),
                ToolFilter {
                    allow: vec![],
                    deny: vec!["ping".to_string()],
                },
                Box::new(InMemoryMcpClient::default().with_tool("ping", json!({"from": "alpha"}))),
            )
            .unwrap();
        manager
            .register_server(
                make_server_config("beta"),
                ToolFilter::default(),
                Box::new(InMemoryMcpClient::default().with_tool("ping", json!({"from": "beta"}))),
            )
            .unwrap();

        let result = manager.call_qualified_tool("mcp__alpha__ping", json!({}));
        if let Ok(value) = result {
            assert_ne!(
                value["from"], "beta",
                "denied tool leaked through to another server"
            );
        }
    }

    #[test]
    fn manager_unregister_removes_server() {
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config("s1"),
                ToolFilter::default(),
                Box::new(InMemoryMcpClient::default()),
            )
            .unwrap();
        manager.unregister_server("s1").unwrap();
        assert!(manager.configs.is_empty());
    }

    #[test]
    fn manager_unregister_errors_on_unknown() {
        let mut manager = McpManager::default();
        let err = manager.unregister_server("nope").unwrap_err();
        assert!(err.to_string().contains("not registered"));
    }

    #[test]
    fn manager_stop_server_errors_on_unknown() {
        let mut manager = McpManager::default();
        let err = manager.stop_server("nope").unwrap_err();
        assert!(err.to_string().contains("not running"));
    }

    #[test]
    fn manager_list_resources_returns_from_clients() {
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config("s1"),
                ToolFilter::default(),
                Box::new(
                    InMemoryMcpClient::default()
                        .with_resource("mcp://s1/health", json!({"ok": true})),
                ),
            )
            .unwrap();
        let resources = manager.list_resources().unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].server_name, "s1");
    }

    #[test]
    fn manager_read_resource_delegates() {
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config("s1"),
                ToolFilter::default(),
                Box::new(
                    InMemoryMcpClient::default()
                        .with_resource("mcp://s1/health", json!({"ok": true})),
                ),
            )
            .unwrap();
        let result = manager.read_resource("s1", "mcp://s1/health").unwrap();
        assert_eq!(result["ok"], true);
    }

    #[test]
    fn manager_update_sandbox_state_returns_notices() {
        let mut manager = McpManager::default();
        manager
            .register_server(
                make_server_config("s1"),
                ToolFilter::default(),
                Box::new(InMemoryMcpClient::default()),
            )
            .unwrap();
        let notices = manager.update_sandbox_state("strict", "/tmp").unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0]["server_name"], "s1");
    }

    // ── Tool filter ────────────────────────────────────────────────────

    #[test]
    fn allowed_by_filter_empty_allow_permits_all() {
        let filter = ToolFilter {
            allow: vec![],
            deny: vec![],
        };
        assert!(allowed_by_filter("anything", &filter));
    }

    #[test]
    fn allowed_by_filter_deny_blocks() {
        let filter = ToolFilter {
            allow: vec![],
            deny: vec!["danger".to_string()],
        };
        assert!(!allowed_by_filter("danger", &filter));
        assert!(allowed_by_filter("safe", &filter));
    }

    #[test]
    fn allowed_by_filter_allow_only_permits_listed() {
        let filter = ToolFilter {
            allow: vec!["a".to_string()],
            deny: vec![],
        };
        assert!(allowed_by_filter("a", &filter));
        assert!(!allowed_by_filter("b", &filter));
    }

    // ── Helper functions ───────────────────────────────────────────────

    #[test]
    fn sanitize_component_lowercases_and_replaces_specials() {
        assert_eq!(sanitize_component("My-Server.Name"), "my_server_name");
        assert_eq!(sanitize_component("ABC123"), "abc123");
    }

    #[test]
    fn qualify_tool_name_produces_mcp_prefix() {
        let name = qualify_tool_name("server", "tool");
        assert!(name.starts_with("mcp__server__tool"));
    }

    #[test]
    fn qualify_tool_name_truncates_long_names() {
        let long_server = "a".repeat(100);
        let name = qualify_tool_name(&long_server, "tool");
        assert!(name.len() <= 64);
        assert!(parse_qualified_tool_name(&name).is_ok());
    }

    #[test]
    fn parse_qualified_tool_name_round_trip() {
        let qualified = qualify_tool_name("my_server", "my_tool");
        let (server, tool) = parse_qualified_tool_name(&qualified).unwrap();
        assert_eq!(server, "my_server");
        assert_eq!(tool, "my_tool");
    }

    #[test]
    fn parse_qualified_tool_name_rejects_missing_prefix() {
        let err = parse_qualified_tool_name("not_mcp__server__tool").unwrap_err();
        assert!(err.to_string().contains("missing mcp__ prefix"));
    }

    #[test]
    fn parse_qualified_tool_name_rejects_empty_segments() {
        let err = parse_qualified_tool_name("mcp____tool").unwrap_err();
        assert!(err.to_string().contains("missing server segment"));
    }

    #[test]
    fn parse_server_from_uri_extracts_server() {
        assert_eq!(
            parse_server_from_uri("mcp://my-server/capabilities"),
            Some("my-server".to_string())
        );
    }

    #[test]
    fn parse_server_from_uri_returns_none_for_invalid() {
        assert!(parse_server_from_uri("http://not-mcp").is_none());
        assert!(parse_server_from_uri("mcp:///path").is_none());
    }

    // ── JsonRpcError ───────────────────────────────────────────────────

    #[test]
    fn jsonrpc_error_codes_are_correct() {
        assert_eq!(JsonRpcError::parse_error("").code, -32700);
        assert_eq!(JsonRpcError::invalid_request("").code, -32600);
        assert_eq!(JsonRpcError::method_not_found("x").code, -32601);
        assert_eq!(JsonRpcError::invalid_params("").code, -32602);
        assert_eq!(JsonRpcError::internal("").code, -32603);
    }

    #[test]
    fn jsonrpc_result_produces_valid_envelope() {
        let result = jsonrpc_result(Some(json!(1)), json!({"ok": true}));
        assert_eq!(result["jsonrpc"], "2.0");
        assert_eq!(result["id"], 1);
        assert_eq!(result["result"]["ok"], true);
    }

    #[test]
    fn jsonrpc_error_produces_valid_envelope() {
        let err = jsonrpc_error(Some(json!(2)), JsonRpcError::invalid_params("bad"));
        assert_eq!(err["jsonrpc"], "2.0");
        assert_eq!(err["id"], 2);
        assert_eq!(err["error"]["code"], -32602);
    }

    #[test]
    fn jsonrpc_notifications_do_not_require_responses() {
        assert!(!should_respond_to_jsonrpc(&None));
        assert!(should_respond_to_jsonrpc(&Some(json!(1))));
    }

    // ── McpServerConfig serialization ──────────────────────────────────

    #[test]
    fn mcp_server_config_defaults_enabled_to_true() {
        let json = json!({"name": "s", "command": "cmd"});
        let config: McpServerConfig = serde_json::from_value(json).unwrap();
        assert!(config.enabled);
        assert!(config.args.is_empty());
        assert!(config.env.is_empty());
    }

    #[test]
    fn mcp_startup_status_serializes_with_snake_case() {
        let status = McpStartupStatus::Failed {
            error: "oops".to_string(),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["failed"]["error"], "oops");
    }

    #[cfg(unix)]
    mod stdio_proxy {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        use std::path::{Path, PathBuf};

        /// A minimal but real MCP server: reads JSON-RPC lines on stdin and answers
        /// with values that the stub client could never produce.
        const FAKE_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{}}}\n' "$id" ;;
    *'"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"spawned_echo","description":"real"}]}}\n' "$id" ;;
    *'"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"proof":"came-from-child","pid_env":"'"$CW_TEST_MARKER"'"}}\n' "$id" ;;
    *'"resources/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"resources":[]}}\n' "$id" ;;
    *) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
  esac
done
"#;

        fn write_fake_server(tag: &str) -> PathBuf {
            let dir =
                std::env::temp_dir().join(format!("cw-mcp-test-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("server.sh");
            std::fs::write(&path, FAKE_SERVER).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        fn definition(path: &Path) -> McpServerDefinition {
            McpServerDefinition {
                config: McpServerConfig {
                    name: "spawned".to_string(),
                    command: path.to_string_lossy().to_string(),
                    args: vec![],
                    env: HashMap::from([(
                        "CW_TEST_MARKER".to_string(),
                        "env-was-passed".to_string(),
                    )]),
                    enabled: true,
                },
                filter: ToolFilter::default(),
            }
        }

        #[test]
        fn stdio_server_proxies_to_the_spawned_process_not_a_stub() {
            let path = write_fake_server("proxy");
            let state = build_stdio_state(vec![definition(&path)]);

            let tools = state.manager.list_tools().unwrap();
            let names: Vec<&str> = tools.iter().map(|t| t.tool_name.as_str()).collect();
            assert_eq!(
                names,
                vec!["spawned_echo"],
                "tools must come from the spawned process, not a hardcoded stub"
            );
            assert!(
                !names.contains(&"health") && !names.contains(&"capabilities"),
                "stub tools must not be advertised: {names:?}"
            );

            let result = state
                .manager
                .call_tool("spawned", "spawned_echo", json!({"x": 1}))
                .unwrap();
            assert_eq!(result["proof"], "came-from-child");
            assert_eq!(
                result["pid_env"], "env-was-passed",
                "configured env must reach the spawned process"
            );
        }

        #[test]
        fn stdio_server_refuses_to_fake_a_server_it_cannot_spawn() {
            let mut definition = definition(&write_fake_server("missing"));
            definition.config.command = "/nonexistent/codewhale-mcp-test-binary".to_string();
            let state = build_stdio_state(vec![definition]);

            assert_eq!(state.running.get("spawned"), Some(&false));
            assert!(
                state.manager.list_tools().unwrap().is_empty(),
                "an unspawnable server must expose no tools at all"
            );
        }
    }
}
