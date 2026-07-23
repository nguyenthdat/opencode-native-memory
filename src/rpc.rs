//! Length-delimited Protobuf service for the private sidecar protocol.

use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use anyhow::{Context, Result, anyhow};
use prost::Message;
use serde_json::{Map, Number, Value as JsonValue, json};

use crate::memory_proto::{Method, Request, Response, Value, ValueList, ValueObject, value};
use crate::{
    DeleteRequest, DoctorRequest, FeedbackRequest, ForgetRequest, GetRequest, ListRequest,
    LockRequest, MemoryConfig, MemoryEngine, PinRequest, PurgeRequest, SearchRequest, StoreRequest,
    SyncSharedRequest, UpdateRequest,
};

/// Incremented because version 2 replaces JSON-lines with Protobuf framing.
pub const RPC_PROTOCOL_VERSION: u32 = 2;
pub const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_VALUE_DEPTH: usize = 64;

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
            .ok_or_else(|| anyhow!("memory engine did not initialize"))
    }

    fn handle(&mut self, request: Request) -> Result<(Response, bool)> {
        let id = request.id;
        let method = Method::try_from(request.method)
            .map_err(|_| anyhow!("unknown memory method value: {}", request.method))?;
        let params = request
            .params
            .as_ref()
            .map(|value| decode_value(value, 0))
            .transpose()?
            .unwrap_or_else(|| json!({}));

        let result = match method {
            Method::Search => serde_json::to_value(
                self.engine()?
                    .search(&serde_json::from_value::<SearchRequest>(params)?)?,
            )?,
            Method::Store => serde_json::to_value(
                self.engine()?
                    .store(serde_json::from_value::<StoreRequest>(params)?)?,
            )?,
            Method::Get => {
                let request = serde_json::from_value::<GetRequest>(params)?;
                serde_json::to_value(self.engine()?.get(&request)?)?
            }
            Method::List => {
                let request = serde_json::from_value::<ListRequest>(params)?;
                serde_json::to_value(self.engine()?.list(&request)?)?
            }
            Method::Update => serde_json::to_value(
                self.engine()?
                    .update(serde_json::from_value::<UpdateRequest>(params)?)?,
            )?,
            Method::Pin => {
                let request = serde_json::from_value::<PinRequest>(params)?;
                serde_json::to_value(self.engine()?.pin(&request)?)?
            }
            Method::Lock => {
                let request = serde_json::from_value::<LockRequest>(params)?;
                serde_json::to_value(self.engine()?.lock(&request)?)?
            }
            Method::Delete => {
                let request = serde_json::from_value::<DeleteRequest>(params)?;
                serde_json::to_value(self.engine()?.delete(&request)?)?
            }
            Method::Forget => {
                let request = serde_json::from_value::<ForgetRequest>(params)?;
                serde_json::to_value(self.engine()?.forget(&request)?)?
            }
            Method::Purge => {
                let request = serde_json::from_value::<PurgeRequest>(params)?;
                serde_json::to_value(self.engine()?.purge(&request)?)?
            }
            Method::Feedback => {
                let request = serde_json::from_value::<FeedbackRequest>(params)?;
                serde_json::to_value(self.engine()?.feedback(&request)?)?
            }
            Method::SyncShared => serde_json::to_value(
                self.engine()?
                    .sync_shared(serde_json::from_value::<SyncSharedRequest>(params)?)?,
            )?,
            Method::Status => serde_json::to_value(self.engine()?.status()?)?,
            Method::Optimize => serde_json::to_value(self.engine()?.optimize()?)?,
            Method::Doctor => {
                let request = serde_json::from_value::<DoctorRequest>(params)?;
                serde_json::to_value(self.engine()?.doctor(&request)?)?
            }
            Method::Shutdown => {
                return Ok((success(id, json!({ "stopped": true }))?, true));
            }
            Method::Unspecified => return Err(anyhow!("memory method is unspecified")),
        };
        Ok((success(id, result)?, false))
    }
}

/// Run a CLI mode or the Protobuf sidecar protocol selected by process args.
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
    input: &mut impl Read,
    output: &mut impl Write,
    config: MemoryConfig,
) -> Result<()> {
    let mut service = Service::new(config);
    loop {
        let Some(frame) = read_frame(input)? else {
            return Ok(());
        };
        let request = match Request::decode(frame.as_slice()) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    output,
                    &failure(0, format!("invalid Protobuf request: {error}")),
                )?;
                continue;
            }
        };
        let request_id = request.id;
        let handled = catch_unwind(AssertUnwindSafe(|| service.handle(request)));
        let (response, shutdown) = match handled {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => (failure(request_id, format!("{error:#}")), false),
            Err(_) => (failure(request_id, "memory operation panicked"), false),
        };
        write_response(output, &response)?;
        if shutdown {
            return Ok(());
        }
    }
}

fn read_frame(input: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let Some(length) = read_varint(input)? else {
        return Ok(None);
    };
    let length = usize::try_from(length).context("Protobuf frame length exceeds usize")?;
    anyhow::ensure!(
        length <= MAX_REQUEST_BYTES,
        "request exceeds {MAX_REQUEST_BYTES} bytes"
    );
    let mut frame = vec![0; length];
    input
        .read_exact(&mut frame)
        .context("truncated Protobuf request frame")?;
    Ok(Some(frame))
}

fn read_varint(input: &mut impl Read) -> Result<Option<u64>> {
    let mut value = 0_u64;
    for shift in (0..70).step_by(7) {
        let mut byte = [0_u8; 1];
        let count = input.read(&mut byte)?;
        if count == 0 {
            anyhow::ensure!(shift == 0, "truncated Protobuf frame length");
            return Ok(None);
        }
        let payload = u64::from(byte[0] & 0x7f);
        anyhow::ensure!(shift < 64 || payload <= 1, "invalid Protobuf frame length");
        value |= payload << shift.min(63);
        if byte[0] & 0x80 == 0 {
            return Ok(Some(value));
        }
    }
    Err(anyhow!("invalid Protobuf frame length"))
}

fn write_response(output: &mut impl Write, response: &Response) -> Result<()> {
    let encoded_len = response.encoded_len();
    anyhow::ensure!(
        encoded_len <= MAX_RESPONSE_BYTES,
        "response exceeds {MAX_RESPONSE_BYTES} bytes"
    );
    let mut frame = Vec::with_capacity(response.encoded_len() + 10);
    response.encode_length_delimited(&mut frame)?;
    output.write_all(&frame)?;
    output.flush()?;
    Ok(())
}

fn success(id: u64, result: JsonValue) -> Result<Response> {
    Ok(Response {
        id,
        ok: true,
        result: Some(encode_value(&result, 0)?),
        error: String::new(),
    })
}

fn failure(id: u64, error: impl Into<String>) -> Response {
    Response {
        id,
        ok: false,
        result: None,
        error: error.into(),
    }
}

fn decode_value(value: &Value, depth: usize) -> Result<JsonValue> {
    anyhow::ensure!(
        depth <= MAX_VALUE_DEPTH,
        "Protobuf value nesting exceeds limit"
    );
    match value.kind.as_ref() {
        Some(value::Kind::BooleanValue(value)) => Ok(JsonValue::Bool(*value)),
        Some(value::Kind::SignedValue(value)) => Ok(JsonValue::Number((*value).into())),
        Some(value::Kind::UnsignedValue(value)) => Ok(JsonValue::Number((*value).into())),
        Some(value::Kind::FloatValue(value)) => Number::from_f64(*value)
            .map(JsonValue::Number)
            .ok_or_else(|| anyhow!("Protobuf value contains a non-finite number")),
        Some(value::Kind::TextValue(value)) => Ok(JsonValue::String(value.clone())),
        Some(value::Kind::ListValue(list)) => list
            .values
            .iter()
            .map(|value| decode_value(value, depth + 1))
            .collect::<Result<Vec<_>>>()
            .map(JsonValue::Array),
        Some(value::Kind::ObjectValue(object)) => object
            .fields
            .iter()
            .map(|(key, value)| Ok((key.clone(), decode_value(value, depth + 1)?)))
            .collect::<Result<Map<_, _>>>()
            .map(JsonValue::Object),
        Some(value::Kind::NullValue(_)) | None => Ok(JsonValue::Null),
    }
}

fn encode_value(value: &JsonValue, depth: usize) -> Result<Value> {
    anyhow::ensure!(
        depth <= MAX_VALUE_DEPTH,
        "response value nesting exceeds limit"
    );
    let kind = match value {
        JsonValue::Null => value::Kind::NullValue(true),
        JsonValue::Bool(value) => value::Kind::BooleanValue(*value),
        JsonValue::Number(value) => {
            if let Some(value) = value.as_i64() {
                value::Kind::SignedValue(value)
            } else if let Some(value) = value.as_u64() {
                value::Kind::UnsignedValue(value)
            } else {
                value::Kind::FloatValue(
                    value
                        .as_f64()
                        .ok_or_else(|| anyhow!("cannot encode response number"))?,
                )
            }
        }
        JsonValue::String(value) => value::Kind::TextValue(value.clone()),
        JsonValue::Array(values) => value::Kind::ListValue(ValueList {
            values: values
                .iter()
                .map(|value| encode_value(value, depth + 1))
                .collect::<Result<Vec<_>>>()?,
        }),
        JsonValue::Object(values) => value::Kind::ObjectValue(ValueObject {
            fields: values
                .iter()
                .map(|(key, value)| Ok((key.clone(), encode_value(value, depth + 1)?)))
                .collect::<Result<HashMap<_, _>>>()?,
        }),
    };
    Ok(Value { kind: Some(kind) })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use prost::Message;
    use serde_json::json;

    use super::{
        Method, RPC_PROTOCOL_VERSION, Request, Service, decode_value, encode_value, read_frame,
        run_protocol_io,
    };
    use crate::MemoryConfig;

    fn config() -> (tempfile::TempDir, MemoryConfig) {
        let temp = tempfile::tempdir().expect("create temp dir");
        let config = MemoryConfig::new(
            temp.path().join("project"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        (temp, config)
    }

    #[test]
    fn protobuf_value_round_trip_preserves_contract_data() {
        let input = json!({
            "text": "memory",
            "enabled": true,
            "count": 7,
            "score": 0.75,
            "items": [null, "ok"]
        });
        let encoded = encode_value(&input, 0).expect("encode value");
        assert_eq!(decode_value(&encoded, 0).expect("decode value"), input);
    }

    #[test]
    fn shutdown_does_not_initialize_the_model() {
        let (_temp, config) = config();
        let mut service = Service::new(config);
        let request = Request {
            id: 7,
            method: Method::Shutdown as i32,
            params: Some(encode_value(&json!({}), 0).expect("encode params")),
        };
        let (response, shutdown) = service.handle(request).expect("handle shutdown");
        assert!(shutdown);
        assert!(response.ok);
        assert!(service.engine.is_none());
    }

    #[test]
    fn protocol_reads_and_writes_length_delimited_messages() {
        let (_temp, config) = config();
        let request = Request {
            id: 9,
            method: Method::Shutdown as i32,
            params: Some(encode_value(&json!({}), 0).expect("encode params")),
        };
        let mut input = Vec::new();
        request
            .encode_length_delimited(&mut input)
            .expect("encode request");
        let mut output = Vec::new();
        run_protocol_io(&mut Cursor::new(input), &mut output, config).expect("run protocol");

        let frame = read_frame(&mut Cursor::new(output))
            .expect("read response")
            .expect("response frame");
        let response = super::Response::decode(frame.as_slice()).expect("decode response");
        assert_eq!(response.id, 9);
        assert!(response.ok);
    }

    #[test]
    fn protocol_version_marks_protobuf_transport() {
        assert_eq!(RPC_PROTOCOL_VERSION, 2);
    }
}
