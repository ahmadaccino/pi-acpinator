//! ACP MCP server configs -> Pi MCP adapter config files.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use agent_client_protocol::schema::v1::{EnvVariable, HttpHeader, McpServer};
use anyhow::{Context, Result};
use serde_json::{Map, Value};

pub(crate) struct PreparedConfig {
    file: Option<ConfigFile>,
    key: Option<u64>,
}

impl PreparedConfig {
    pub(crate) fn path(&self) -> Option<&Path> {
        self.file.as_ref().map(ConfigFile::path)
    }

    pub(crate) fn into_parts(self) -> (Option<ConfigFile>, Option<u64>) {
        (self.file, self.key)
    }
}

/// Owns a temporary Pi MCP config file and removes it with the session.
pub(crate) struct ConfigFile {
    path: PathBuf,
}

impl ConfigFile {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(crate) fn prepare(servers: &[McpServer]) -> Result<PreparedConfig> {
    let Some(text) = config_text(servers)? else {
        return Ok(PreparedConfig {
            file: None,
            key: None,
        });
    };
    let key = config_text_key(&text);
    Ok(PreparedConfig {
        file: Some(write_temp_config(&text)?),
        key: Some(key),
    })
}

pub(crate) fn config_key(servers: &[McpServer]) -> Result<Option<u64>> {
    config_text(servers).map(|text| text.as_deref().map(config_text_key))
}

fn config_text_key(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn config_text(servers: &[McpServer]) -> Result<Option<String>> {
    if servers.is_empty() {
        return Ok(None);
    }
    let value = config_value(servers)?;
    Ok(Some(format!("{}\n", serde_json::to_string_pretty(&value)?)))
}

fn config_value(servers: &[McpServer]) -> Result<Value> {
    let mut used_names = HashSet::new();
    let mut mcp_servers = Map::new();
    for server in servers {
        let (name, entry) = server_entry(server)?;
        mcp_servers.insert(unique_name(&name, &mut used_names), entry);
    }

    let mut root = Map::new();
    root.insert("mcpServers".to_string(), Value::Object(mcp_servers));
    Ok(Value::Object(root))
}

fn server_entry(server: &McpServer) -> Result<(String, Value)> {
    let mut entry = Map::new();
    match server {
        McpServer::Stdio(stdio) => {
            entry.insert(
                "command".to_string(),
                Value::String(stdio.command.to_string_lossy().into_owned()),
            );
            if !stdio.args.is_empty() {
                entry.insert(
                    "args".to_string(),
                    Value::Array(stdio.args.iter().cloned().map(Value::String).collect()),
                );
            }
            if !stdio.env.is_empty() {
                entry.insert("env".to_string(), env_map(&stdio.env));
            }
            Ok((stdio.name.clone(), Value::Object(entry)))
        }
        McpServer::Http(http) => {
            entry.insert("url".to_string(), Value::String(http.url.clone()));
            if !http.headers.is_empty() {
                entry.insert("headers".to_string(), header_map(&http.headers));
            }
            Ok((http.name.clone(), Value::Object(entry)))
        }
        McpServer::Sse(sse) => {
            entry.insert("url".to_string(), Value::String(sse.url.clone()));
            if !sse.headers.is_empty() {
                entry.insert("headers".to_string(), header_map(&sse.headers));
            }
            Ok((sse.name.clone(), Value::Object(entry)))
        }
        _ => anyhow::bail!("unsupported MCP server transport"),
    }
}

fn env_map(env: &[EnvVariable]) -> Value {
    let mut out = Map::new();
    for var in env {
        out.insert(var.name.clone(), Value::String(var.value.clone()));
    }
    Value::Object(out)
}

fn header_map(headers: &[HttpHeader]) -> Value {
    let mut out = Map::new();
    for header in headers {
        out.insert(header.name.clone(), Value::String(header.value.clone()));
    }
    Value::Object(out)
}

fn unique_name(name: &str, used: &mut HashSet<String>) -> String {
    let base = match name.trim() {
        "" => "mcp-server",
        trimmed => trimmed,
    };
    if used.insert(base.to_string()) {
        return base.to_string();
    }

    for n in 2.. {
        let candidate = format!("{base}-{n}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!()
}

fn write_temp_config(contents: &str) -> Result<ConfigFile> {
    let path = std::env::temp_dir().join(format!("pi-acpinator-mcp-{}.json", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let write_result = (|| -> Result<()> {
        let mut file = options
            .open(&path)
            .with_context(|| format!("failed to create MCP config at {}", path.display()))?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("failed to write MCP config at {}", path.display()))?;
        Ok(())
    })();
    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&path);
        return Err(err);
    }
    Ok(ConfigFile { path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{McpServerHttp, McpServerSse, McpServerStdio};

    #[test]
    fn builds_pi_config_for_supported_transports() {
        let value = config_value(&[
            McpServer::Stdio(
                McpServerStdio::new("tools", "/bin/echo")
                    .args(vec!["hello".to_string()])
                    .env(vec![EnvVariable::new("TOKEN", "secret")]),
            ),
            McpServer::Http(
                McpServerHttp::new("web", "http://127.0.0.1:3000/mcp")
                    .headers(vec![HttpHeader::new("Authorization", "Bearer token")]),
            ),
            McpServer::Sse(McpServerSse::new("events", "http://127.0.0.1:3001/sse")),
        ])
        .unwrap();

        assert_eq!(
            value["mcpServers"]["tools"],
            serde_json::json!({
                "command": "/bin/echo",
                "args": ["hello"],
                "env": {"TOKEN": "secret"}
            })
        );
        assert_eq!(
            value["mcpServers"]["web"],
            serde_json::json!({
                "url": "http://127.0.0.1:3000/mcp",
                "headers": {"Authorization": "Bearer token"}
            })
        );
        assert_eq!(
            value["mcpServers"]["events"],
            serde_json::json!({"url": "http://127.0.0.1:3001/sse"})
        );
    }

    #[test]
    fn dedupes_server_names() {
        let value = config_value(&[
            McpServer::Stdio(McpServerStdio::new("tools", "/bin/a")),
            McpServer::Stdio(McpServerStdio::new("tools", "/bin/b")),
            McpServer::Stdio(McpServerStdio::new("", "/bin/c")),
        ])
        .unwrap();

        assert_eq!(
            value["mcpServers"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec!["tools", "tools-2", "mcp-server"]
        );
    }

    #[test]
    fn empty_servers_need_no_config() {
        assert!(config_key(&[]).unwrap().is_none());
        assert!(prepare(&[]).unwrap().path().is_none());
    }
}
