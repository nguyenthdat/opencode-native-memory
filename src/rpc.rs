//! Newline-delimited JSON RPC service for the private sidecar protocol.

use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    DeleteRequest, DoctorRequest, FeedbackRequest, ForgetRequest, GetRequest, ListRequest,
    LockRequest, MemoryConfig, MemoryEngine, PinRequest, PurgeRequest, SearchRequest, StoreRequest,
    SyncSharedRequest, UpdateRequest,
};

pub const RPC_PROTOCOL_VERSION: u32 = 1;

/// Maximum encoded request-frame size, including the trailing newline.
///
/// This value is part of the private sidecar contract and must match the
/// TypeScript client's `MAX_REQUEST_BYTES`.
pub const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcRequest {
    id: u64,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct RpcResponse {
    id: u64,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl RpcResponse {
    fn success(id: u64, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    fn failure(id: u64, error: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum RpcMethod {
    Search,
    Store,
    Get,
    List,
    Update,
    Pin,
    Lock,
    Delete,
    Forget,
    Purge,
    Feedback,
    SyncShared,
    Status,
    Optimize,
    Doctor,
    Shutdown,
}

impl RpcMethod {
    fn parse(method: &str) -> Result<Self> {
        match method {
            "search" => Ok(Self::Search),
            "store" => Ok(Self::Store),
            "get" => Ok(Self::Get),
            "list" => Ok(Self::List),
            "update" => Ok(Self::Update),
            "pin" => Ok(Self::Pin),
            "lock" => Ok(Self::Lock),
            "delete" => Ok(Self::Delete),
            "forget" => Ok(Self::Forget),
            "purge" => Ok(Self::Purge),
            "feedback" => Ok(Self::Feedback),
            "sync_shared" => Ok(Self::SyncShared),
            "status" => Ok(Self::Status),
            "optimize" => Ok(Self::Optimize),
            "doctor" => Ok(Self::Doctor),
            "shutdown" => Ok(Self::Shutdown),
            _ => Err(anyhow!("unknown native memory method: {method}")),
        }
    }
}

struct Service {
    config: MemoryConfig,
    engine: Option<MemoryEngine>,
}

impl Service {
    fn new(config: MemoryConfig) -> Self {
        Self {
            config,
            engine: None,
        }
    }

    fn engine(&mut self) -> Result<&mut MemoryEngine> {
        if self.engine.is_none() {
            self.engine = Some(MemoryEngine::open(self.config.clone())?);
        }
        self.engine
            .as_mut()
            .ok_or_else(|| anyhow!("native memory engine did not initialize"))
    }

    fn handle(&mut self, request: RpcRequest) -> Result<(RpcResponse, bool)> {
        let id = request.id;
        let method = RpcMethod::parse(&request.method)?;
        let result = match method {
            RpcMethod::Search => {
                let params = serde_json::from_value::<SearchRequest>(request.params)?;
                serde_json::to_value(self.engine()?.search(&params)?)?
            }
            RpcMethod::Store => serde_json::to_value(
                self.engine()?
                    .store(serde_json::from_value::<StoreRequest>(request.params)?)?,
            )?,
            RpcMethod::Get => {
                let params = serde_json::from_value::<GetRequest>(request.params)?;
                serde_json::to_value(self.engine()?.get(&params)?)?
            }
            RpcMethod::List => {
                let params = serde_json::from_value::<ListRequest>(request.params)?;
                serde_json::to_value(self.engine()?.list(&params)?)?
            }
            RpcMethod::Update => serde_json::to_value(
                self.engine()?
                    .update(serde_json::from_value::<UpdateRequest>(request.params)?)?,
            )?,
            RpcMethod::Pin => serde_json::to_value(
                self.engine()?
                    .pin(&serde_json::from_value::<PinRequest>(request.params)?)?,
            )?,
            RpcMethod::Lock => serde_json::to_value(
                self.engine()?
                    .lock(&serde_json::from_value::<LockRequest>(request.params)?)?,
            )?,
            RpcMethod::Delete => {
                let params = serde_json::from_value::<DeleteRequest>(request.params)?;
                serde_json::to_value(self.engine()?.delete(&params)?)?
            }
            RpcMethod::Forget => {
                let params = serde_json::from_value::<ForgetRequest>(request.params)?;
                serde_json::to_value(self.engine()?.forget(&params)?)?
            }
            RpcMethod::Purge => {
                let params = serde_json::from_value::<PurgeRequest>(request.params)?;
                serde_json::to_value(self.engine()?.purge(&params)?)?
            }
            RpcMethod::Feedback => {
                let params = serde_json::from_value::<FeedbackRequest>(request.params)?;
                serde_json::to_value(self.engine()?.feedback(&params)?)?
            }
            RpcMethod::SyncShared => {
                serde_json::to_value(self.engine()?.sync_shared(serde_json::from_value::<
                    SyncSharedRequest,
                >(
                    request.params
                )?)?)?
            }
            RpcMethod::Status => serde_json::to_value(self.engine()?.status()?)?,
            RpcMethod::Optimize => serde_json::to_value(self.engine()?.optimize()?)?,
            RpcMethod::Doctor => {
                let params = serde_json::from_value::<DoctorRequest>(request.params)?;
                serde_json::to_value(self.engine()?.doctor(&params)?)?
            }
            RpcMethod::Shutdown => {
                return Ok((RpcResponse::success(id, json!({ "stopped": true })), true));
            }
        };
        Ok((RpcResponse::success(id, result), false))
    }
}

/// Run the CLI mode or private JSON-line protocol selected by process args.
///
/// # Errors
///
/// Returns an error when configuration, storage, model initialization, request
/// I/O, or a requested operation fails.
pub fn run() -> Result<()> {
    let config = MemoryConfig::discover()?;
    match std::env::args().nth(1).as_deref() {
        Some("--doctor") => {
            let engine = MemoryEngine::open(config)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&engine.doctor(&DoctorRequest { deep: true })?)?
            );
            Ok(())
        }
        Some("--warmup") => {
            let engine = MemoryEngine::open(config)?;
            println!("{}", serde_json::to_string_pretty(&engine.status()?)?);
            Ok(())
        }
        Some(argument) => Err(anyhow!("unknown argument: {argument}")),
        None => run_protocol(config),
    }
}

fn run_protocol(config: MemoryConfig) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = BufReader::new(stdin.lock());
    let mut output = BufWriter::new(stdout.lock());
    run_protocol_io(&mut input, &mut output, config)
}

fn run_protocol_io(
    input: &mut impl BufRead,
    output: &mut impl Write,
    config: MemoryConfig,
) -> Result<()> {
    let mut service = Service::new(config);
    let mut buffer = Vec::new();

    loop {
        match read_request_frame(input, &mut buffer)? {
            FrameStatus::Eof => break,
            FrameStatus::Oversized => {
                write_response(
                    output,
                    &RpcResponse::failure(0, format!("request exceeds {MAX_REQUEST_BYTES} bytes")),
                )?;
                continue;
            }
            FrameStatus::Complete => {}
        }

        let request = match serde_json::from_slice::<RpcRequest>(&buffer) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    output,
                    &RpcResponse::failure(0, format!("invalid request JSON: {error}")),
                )?;
                continue;
            }
        };
        let request_id = request.id;
        let handled = catch_unwind(AssertUnwindSafe(|| service.handle(request)));
        let (response, shutdown) = match handled {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => (
                RpcResponse::failure(request_id, format!("{error:#}")),
                false,
            ),
            Err(_) => (
                RpcResponse::failure(request_id, "native memory operation panicked"),
                false,
            ),
        };
        write_response(output, &response)?;
        if shutdown {
            break;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum FrameStatus {
    Eof,
    Complete,
    Oversized,
}

fn read_request_frame(input: &mut impl BufRead, buffer: &mut Vec<u8>) -> io::Result<FrameStatus> {
    buffer.clear();
    let mut oversized = false;

    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            return if oversized {
                Ok(FrameStatus::Oversized)
            } else if buffer.is_empty() {
                Ok(FrameStatus::Eof)
            } else {
                Ok(FrameStatus::Complete)
            };
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let consume = newline.map_or(available.len(), |index| index + 1);
        if !oversized {
            let remaining = MAX_REQUEST_BYTES.saturating_sub(buffer.len());
            if consume <= remaining {
                buffer.extend_from_slice(&available[..consume]);
            } else {
                buffer.extend_from_slice(&available[..remaining]);
                oversized = true;
            }
        }
        input.consume(consume);

        if newline.is_some() {
            return if oversized {
                Ok(FrameStatus::Oversized)
            } else {
                Ok(FrameStatus::Complete)
            };
        }
    }
}

fn write_response(output: &mut impl Write, response: &RpcResponse) -> Result<()> {
    serde_json::to_writer(&mut *output, response)
        .context("cannot encode native memory response")?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use serde_json::{json, Value};

    use super::{
        read_request_frame, run_protocol_io, FrameStatus, RpcMethod, RpcRequest, Service,
        MAX_REQUEST_BYTES, RPC_PROTOCOL_VERSION,
    };
    use crate::contract::{IndexStatus, StatusResponse};
    use crate::MemoryConfig;

    #[test]
    fn dispatch_table_preserves_private_method_names() {
        for method in [
            "search",
            "store",
            "get",
            "list",
            "update",
            "pin",
            "lock",
            "delete",
            "forget",
            "purge",
            "feedback",
            "sync_shared",
            "status",
            "optimize",
            "doctor",
            "shutdown",
        ] {
            assert!(RpcMethod::parse(method).is_ok(), "missing method {method}");
        }
        assert!(RpcMethod::parse("export").is_err());
    }

    #[test]
    fn shutdown_dispatch_does_not_initialize_the_model() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let config = MemoryConfig::new(
            temp.path().join("project"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let mut service = Service::new(config);

        let (response, shutdown) = service
            .handle(RpcRequest {
                id: 7,
                method: "shutdown".to_string(),
                params: Value::Null,
            })
            .expect("dispatch shutdown");

        assert!(shutdown);
        assert!(response.ok);
        assert!(service.engine.is_none());
    }

    #[test]
    fn status_serializes_protocol_version() {
        let status = StatusResponse {
            ready: true,
            rpc_protocol_version: RPC_PROTOCOL_VERSION,
            backend: "zvec",
            zvec_version: "test".to_string(),
            embedding_model: "test-model",
            embedding_dimension: 384,
            project_root: "/project".to_string(),
            project_id: "project".to_string(),
            collection_path: "/data/zvec".to_string(),
            document_count: 0,
            state_schema_version: 3,
            metadata_count: 0,
            tombstone_count: 0,
            retrieval_count: 0,
            indexes: vec![IndexStatus {
                name: "embedding".to_string(),
                completeness: 1.0,
            }],
            capabilities: vec!["phase1_taxonomy_lifecycle_v1"],
        };

        let value = serde_json::to_value(&status).expect("serialize status");
        assert_eq!(value["rpc_protocol_version"], json!(1));
        assert_eq!(value["state_schema_version"], json!(3));
        assert_eq!(
            value["capabilities"],
            json!(["phase1_taxonomy_lifecycle_v1"])
        );
    }

    #[test]
    fn request_framer_accepts_an_exact_limit_frame() {
        let frame = shutdown_frame(7, MAX_REQUEST_BYTES);
        let mut input = BufReader::with_capacity(4_096, Cursor::new(frame));
        let mut buffer = Vec::new();

        let status = read_request_frame(&mut input, &mut buffer).expect("read exact-limit frame");
        let request = serde_json::from_slice::<RpcRequest>(&buffer).expect("parse exact frame");

        assert_eq!(status, FrameStatus::Complete);
        assert_eq!(buffer.len(), MAX_REQUEST_BYTES);
        assert_eq!(request.id, 7);
        assert_eq!(request.method, "shutdown");
    }

    #[test]
    fn oversized_frame_is_drained_before_following_shutdown() {
        let mut input_bytes = vec![b'x'; MAX_REQUEST_BYTES + 1];
        *input_bytes.last_mut().expect("oversized frame byte") = b'\n';
        input_bytes.extend(shutdown_frame(9, 64));
        let mut input = BufReader::with_capacity(4_096, Cursor::new(input_bytes));
        let mut output = Vec::new();
        let temp = tempfile::tempdir().expect("create temp dir");
        let config = MemoryConfig::new(
            temp.path().join("project"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );

        run_protocol_io(&mut input, &mut output, config).expect("run bounded protocol");

        let responses = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).expect("parse response"))
            .collect::<Vec<_>>();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["id"], json!(0));
        assert_eq!(responses[0]["ok"], json!(false));
        assert!(responses[0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("request exceeds")));
        assert_eq!(responses[1]["id"], json!(9));
        assert_eq!(responses[1]["ok"], json!(true));
        assert_eq!(responses[1]["result"]["stopped"], json!(true));
    }

    fn shutdown_frame(id: u64, total_bytes: usize) -> Vec<u8> {
        let prefix = format!("{{\"id\":{id},\"method\":\"shutdown\",\"params\":\"");
        let suffix = b"\"}\n";
        assert!(total_bytes >= prefix.len() + suffix.len());
        let padding = total_bytes - prefix.len() - suffix.len();
        let mut frame = Vec::with_capacity(total_bytes);
        frame.extend_from_slice(prefix.as_bytes());
        frame.extend(std::iter::repeat_n(b'x', padding));
        frame.extend_from_slice(suffix);
        frame
    }
}
