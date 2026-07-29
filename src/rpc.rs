//! Domain dispatch and legacy stdio transport for native memory.

use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use prost::Message;
use serde_json::{Map, Number, Value as JsonValue, json};

use crate::memory_proto::{Method, Request, Response, Value, ValueList, ValueObject, value};
use crate::model::{self, ModelSwitchRequest};
use crate::model_proto::{
    EmbeddingMetric, EmbeddingModality,
    ListModelProfilesResponse as ProtoListModelProfilesResponse, ModelPreflightDecision,
    ModelProfile as ProtoModelProfile, ModelProfileCapability, ModelProfileReason,
    ModelProfileRole, ModelProfileSupportLevel, ModelRequest, ModelResponse, ModelStatus,
    ModelStatusCode, ModelSwitchAvailability, ModelSwitchBlocker as ProtoModelSwitchBlocker,
    ModelSwitchExecutionMode, ModelSwitchPreflight as ProtoModelSwitchPreflight,
    ModelSwitchRebuildPolicy, ModelSwitchState, StartModelSwitchResponse, model_request,
    model_response,
};
use crate::{
    CaptureRequest, DeleteRequest, DoctorRequest, DocumentIndexRequest, ExportRequest,
    FeedbackRequest, ForgetRequest, GetRequest, ImportRequest, IngestRequest, ListRequest,
    LockRequest, MemoryConfig, MemoryEngine, PinRequest, PurgeRequest, SearchRequest, StoreRequest,
    SyncSharedRequest, UpdateRequest,
};

/// Incremented because version 2 replaces JSON-lines with Protobuf framing.
pub const RPC_PROTOCOL_VERSION: u32 = 2;
pub const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_VALUE_DEPTH: usize = 64;
const MAX_MODEL_ID_BYTES: usize = 128;

pub(crate) enum ProjectRequest {
    Memory(Request),
    Model(ModelRequest),
}

pub(crate) enum ProjectResponse {
    Memory(Response),
    Model(ModelResponse),
}

pub(crate) struct Service {
    config: MemoryConfig,
    engine: Option<MemoryEngine>,
    model_load_lock: Arc<Mutex<()>>,
    inference_lock: Arc<Mutex<()>>,
}

impl Service {
    pub(crate) fn new(config: MemoryConfig) -> Self {
        Self::new_with_locks(config, Arc::new(Mutex::new(())), Arc::new(Mutex::new(())))
    }

    pub(crate) fn new_with_locks(
        config: MemoryConfig,
        model_load_lock: Arc<Mutex<()>>,
        inference_lock: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            config,
            engine: None,
            model_load_lock,
            inference_lock,
        }
    }

    fn engine(&mut self) -> Result<&mut MemoryEngine> {
        if self.engine.is_none() {
            let _model_load_guard = self
                .model_load_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.engine = Some(MemoryEngine::open_with_inference_lock(
                self.config.clone(),
                Arc::clone(&self.inference_lock),
            )?);
        }
        self.engine
            .as_mut()
            .ok_or_else(|| anyhow!("memory engine did not initialize"))
    }

    pub(crate) fn prepare_project_request(&mut self, request: &ProjectRequest) -> Result<()> {
        if matches!(request, ProjectRequest::Memory(_)) {
            self.engine()?;
        }
        Ok(())
    }

    pub(crate) fn setup_failure_response(
        request: &ProjectRequest,
        error: &anyhow::Error,
    ) -> ProjectResponse {
        match request {
            ProjectRequest::Memory(request) => ProjectResponse::Memory(failure(
                request.id,
                format!("memory engine initialization failed: {error:#}"),
            )),
            ProjectRequest::Model(request) => ProjectResponse::Model(model_error(
                request.id,
                ModelStatusCode::Internal,
                format!("model service initialization failed: {error:#}"),
            )),
        }
    }

    pub(crate) fn handle(&mut self, request: Request) -> Result<(Response, bool)> {
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
            Method::Capture => serde_json::to_value(
                self.engine()?
                    .capture(serde_json::from_value::<CaptureRequest>(params)?)?,
            )?,
            Method::Export => {
                let request = serde_json::from_value::<ExportRequest>(params)?;
                serde_json::to_value(self.engine()?.export_snapshot(&request)?)?
            }
            Method::Import => serde_json::to_value(
                self.engine()?
                    .import_snapshot(serde_json::from_value::<ImportRequest>(params)?)?,
            )?,
            Method::Ingest => serde_json::to_value(
                self.engine()?
                    .ingest(serde_json::from_value::<IngestRequest>(params)?)?,
            )?,
            Method::IndexDocuments => serde_json::to_value(
                self.engine()?
                    .index_documents(&serde_json::from_value::<DocumentIndexRequest>(params)?)?,
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

    pub(crate) fn handle_project(
        &mut self,
        request: ProjectRequest,
    ) -> Result<(ProjectResponse, bool)> {
        match request {
            ProjectRequest::Memory(request) => self
                .handle(request)
                .map(|(response, shutdown)| (ProjectResponse::Memory(response), shutdown)),
            ProjectRequest::Model(request) => {
                Ok((ProjectResponse::Model(self.handle_model(request)), false))
            }
        }
    }

    fn handle_model(&mut self, request: ModelRequest) -> ModelResponse {
        let id = request.id;
        if let Err(error) = validate_model_request(&request) {
            return model_error(id, ModelStatusCode::InvalidArgument, error.to_string());
        }
        let Some(operation) = request.operation else {
            return model_error(
                id,
                ModelStatusCode::InvalidArgument,
                "model request operation is required",
            );
        };

        match operation {
            model_request::Operation::ListProfiles(_) => {
                match model_profiles_response(model::profiles(&self.config)) {
                    Ok(response) => model_ok(id, model_response::Result::ListProfiles(response)),
                    Err(error) => model_error(id, ModelStatusCode::Internal, error.to_string()),
                }
            }
            model_request::Operation::StartSwitch(request) => self.handle_model_switch(id, request),
            model_request::Operation::GetSwitchStatus(_) => model_error(
                id,
                ModelStatusCode::Unavailable,
                "durable model switch status is not enabled in this phase",
            ),
            model_request::Operation::CancelSwitch(_) => model_error(
                id,
                ModelStatusCode::Unavailable,
                "durable model switch cancellation is not enabled in this phase",
            ),
        }
    }

    fn handle_model_switch(
        &self,
        id: u64,
        request: crate::model_proto::StartModelSwitchRequest,
    ) -> ModelResponse {
        let execution_mode = match ModelSwitchExecutionMode::try_from(request.execution_mode) {
            Ok(value) => value,
            Err(_) => {
                return model_error(
                    id,
                    ModelStatusCode::InvalidArgument,
                    "model switch execution_mode is unknown",
                );
            }
        };
        let switch_request = match model_switch_request(request) {
            Ok(request) => request,
            Err(error) => {
                return model_error(id, ModelStatusCode::InvalidArgument, error.to_string());
            }
        };
        if execution_mode == ModelSwitchExecutionMode::Apply {
            return model_error(
                id,
                ModelStatusCode::Unavailable,
                "durable model generation migration is not enabled; use dry-run preflight",
            );
        }

        match model::preflight(&self.config, &switch_request) {
            Ok(response) => match model_switch_response(response) {
                Ok(response) => model_ok(id, model_response::Result::StartSwitch(response)),
                Err(error) => model_error(id, ModelStatusCode::Internal, error.to_string()),
            },
            Err(error) => {
                let code = if error.to_string().starts_with("PROFILE_NOT_FOUND:") {
                    ModelStatusCode::NotFound
                } else {
                    ModelStatusCode::Internal
                };
                model_error(id, code, error.to_string())
            }
        }
    }
}

pub(crate) fn validate_project_request(request: &ProjectRequest) -> Result<()> {
    match request {
        ProjectRequest::Memory(request) => {
            anyhow::ensure!(request.id != 0, "memory request id is required");
            let method = Method::try_from(request.method)
                .map_err(|_| anyhow!("unknown memory method value: {}", request.method))?;
            anyhow::ensure!(
                method != Method::Unspecified,
                "memory method is unspecified"
            );
            Ok(())
        }
        ProjectRequest::Model(request) => validate_model_request(request),
    }
}

fn validate_model_request(request: &ModelRequest) -> Result<()> {
    anyhow::ensure!(request.id != 0, "model request id is required");
    let Some(operation) = request.operation.as_ref() else {
        anyhow::bail!("model request operation is required");
    };
    match operation {
        model_request::Operation::ListProfiles(_) => Ok(()),
        model_request::Operation::StartSwitch(request) => {
            validate_model_id("target_profile_id", &request.target_profile_id)?;
            if let Some(value) = request.switch_id.as_deref() {
                validate_model_id("switch_id", value)?;
            }
            if let Some(value) = request.expected_active_profile_id.as_deref() {
                validate_model_id("expected_active_profile_id", value)?;
            }
            let execution_mode = ModelSwitchExecutionMode::try_from(request.execution_mode)
                .map_err(|_| anyhow!("model switch execution_mode is unknown"))?;
            anyhow::ensure!(
                execution_mode != ModelSwitchExecutionMode::Unspecified,
                "model switch execution_mode is required"
            );
            ModelSwitchAvailability::try_from(request.availability)
                .map_err(|_| anyhow!("model switch availability is unknown"))?;
            ModelSwitchRebuildPolicy::try_from(request.rebuild_policy)
                .map_err(|_| anyhow!("model switch rebuild_policy is unknown"))?;
            Ok(())
        }
        model_request::Operation::GetSwitchStatus(request) => {
            validate_model_id("switch_id", &request.switch_id)
        }
        model_request::Operation::CancelSwitch(request) => {
            validate_model_id("switch_id", &request.switch_id)
        }
    }
}

fn validate_model_id(name: &str, value: &str) -> Result<()> {
    anyhow::ensure!(!value.trim().is_empty(), "model {name} is required");
    anyhow::ensure!(
        value.len() <= MAX_MODEL_ID_BYTES,
        "model {name} is too long"
    );
    anyhow::ensure!(
        !value.chars().any(char::is_control),
        "model {name} contains a control character"
    );
    Ok(())
}

fn model_switch_request(
    request: crate::model_proto::StartModelSwitchRequest,
) -> Result<ModelSwitchRequest> {
    let availability = ModelSwitchAvailability::try_from(request.availability)
        .map_err(|_| anyhow!("model switch availability is unknown"))?;
    let rebuild_policy = ModelSwitchRebuildPolicy::try_from(request.rebuild_policy)
        .map_err(|_| anyhow!("model switch rebuild_policy is unknown"))?;
    let execution_mode = ModelSwitchExecutionMode::try_from(request.execution_mode)
        .map_err(|_| anyhow!("model switch execution_mode is unknown"))?;
    Ok(ModelSwitchRequest {
        target_profile_id: request.target_profile_id,
        switch_id: request.switch_id,
        expected_active_profile_id: request.expected_active_profile_id,
        allow_dense_downtime: matches!(availability, ModelSwitchAvailability::AllowDenseDowntime),
        dry_run: matches!(execution_mode, ModelSwitchExecutionMode::DryRun),
        force_rebuild: matches!(rebuild_policy, ModelSwitchRebuildPolicy::ForceRebuild),
    })
}

fn model_profiles_response(
    response: model::ModelProfilesResponse,
) -> Result<ProtoListModelProfilesResponse> {
    Ok(ProtoListModelProfilesResponse {
        catalog_version: response.catalog_version,
        catalog_digest: response.catalog_digest,
        active_profile_id: response.active_profile_id,
        active_generation_id: response.active_generation_id,
        profiles: response
            .profiles
            .into_iter()
            .map(model_profile)
            .collect::<Result<Vec<_>>>()?,
    })
}

fn model_profile(profile: model::ModelProfile) -> Result<ProtoModelProfile> {
    let modalities = profile
        .modalities
        .iter()
        .map(|value| match value.as_str() {
            "text" => Ok(EmbeddingModality::Text as i32),
            "image" => Ok(EmbeddingModality::Image as i32),
            "mixed" => Ok(EmbeddingModality::Mixed as i32),
            _ => Err(anyhow!("unknown embedding modality: {value}")),
        })
        .collect::<Result<Vec<_>>>()?;
    let metric = match profile.metric.as_deref() {
        None => EmbeddingMetric::Unspecified,
        Some("cosine") => EmbeddingMetric::Cosine,
        Some("dot_product") => EmbeddingMetric::DotProduct,
        Some(value) => return Err(anyhow!("unknown embedding metric: {value}")),
    };
    let support_level = match profile.support_level.as_str() {
        "stable" => ModelProfileSupportLevel::Stable,
        "preview" => ModelProfileSupportLevel::Preview,
        "unsupported" => ModelProfileSupportLevel::Unsupported,
        value => return Err(anyhow!("unknown model support level: {value}")),
    };
    let mut roles = Vec::new();
    if profile.default_for_new_projects {
        roles.push(ModelProfileRole::DefaultForNewProjects as i32);
    }
    if profile.recommended {
        roles.push(ModelProfileRole::Recommended as i32);
    }
    let mut capabilities = Vec::new();
    for (enabled, capability) in [
        (profile.selectable, ModelProfileCapability::Selectable),
        (profile.installed, ModelProfileCapability::Installed),
        (
            profile.platform_supported,
            ModelProfileCapability::PlatformSupported,
        ),
        (
            profile.runtime_available,
            ModelProfileCapability::RuntimeAvailable,
        ),
        (
            profile.artifact_locked,
            ModelProfileCapability::ArtifactLocked,
        ),
    ] {
        if enabled {
            capabilities.push(capability as i32);
        }
    }
    Ok(ProtoModelProfile {
        profile_id: profile.profile_id,
        display_name: profile.display_name,
        description: profile.description,
        modalities,
        repository: profile.repository,
        filename: profile.filename,
        revision: profile.revision,
        artifact_sha256: profile.artifact_sha256,
        runtime_family: profile.runtime_family,
        dimension: profile
            .dimension
            .map(u32::try_from)
            .transpose()
            .context("model profile dimension exceeds uint32")?,
        metric: metric as i32,
        support_level: support_level as i32,
        roles,
        capabilities,
        estimated_download_bytes: profile.estimated_download_bytes,
        estimated_resident_bytes: profile.estimated_resident_bytes,
        unavailable_reason: profile.unavailable_reason.map(|reason| ModelProfileReason {
            code: reason.code,
            message: reason.message,
        }),
    })
}

fn model_switch_response(response: model::ModelSwitchResponse) -> Result<StartModelSwitchResponse> {
    let execution_mode = if response.dry_run {
        ModelSwitchExecutionMode::DryRun
    } else {
        ModelSwitchExecutionMode::Apply
    };
    let state = match response.state.as_str() {
        "preflight" => ModelSwitchState::Preflight,
        value => return Err(anyhow!("unknown model switch state: {value}")),
    };
    Ok(StartModelSwitchResponse {
        switch_id: response.switch_id,
        execution_mode: execution_mode as i32,
        state: state as i32,
        active_profile_id: response.active_profile_id,
        target_profile_id: response.target_profile_id,
        active_generation_id: response.active_generation_id,
        target_generation_id: response.target_generation_id,
        dense_search_available: response.dense_search_available,
        preflight: Some(model_switch_preflight(response.preflight)?),
    })
}

fn model_switch_preflight(
    preflight: model::ModelSwitchPreflight,
) -> Result<ProtoModelSwitchPreflight> {
    let availability = match preflight.availability.as_str() {
        "keep_old_dense" => ModelSwitchAvailability::KeepOldDense,
        "allow_dense_downtime" => ModelSwitchAvailability::AllowDenseDowntime,
        value => return Err(anyhow!("unknown model switch availability: {value}")),
    };
    Ok(ProtoModelSwitchPreflight {
        decision: if preflight.can_start {
            ModelPreflightDecision::Ready as i32
        } else {
            ModelPreflightDecision::Blocked as i32
        },
        availability: availability as i32,
        blockers: preflight
            .blockers
            .into_iter()
            .map(|blocker| ProtoModelSwitchBlocker {
                code: blocker.code,
                message: blocker.message,
            })
            .collect(),
        warnings: preflight.warnings,
        estimated_download_bytes: preflight.estimated_download_bytes,
        estimated_disk_bytes: preflight.estimated_disk_bytes,
        estimated_resident_bytes: preflight.estimated_resident_bytes,
        dense_search_available: preflight.dense_search_available,
    })
}

fn model_ok(id: u64, result: model_response::Result) -> ModelResponse {
    ModelResponse {
        id,
        status: Some(ModelStatus {
            code: ModelStatusCode::Ok as i32,
            message: String::new(),
        }),
        result: Some(result),
    }
}

fn model_error(id: u64, code: ModelStatusCode, message: impl Into<String>) -> ModelResponse {
    ModelResponse {
        id,
        status: Some(ModelStatus {
            code: code as i32,
            message: message.into(),
        }),
        result: None,
    }
}

/// Run daemon mode, a CLI mode, or the legacy stdio protocol selected by process args.
pub fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--daemon") => {
            anyhow::ensure!(
                args.next().as_deref() == Some("--endpoint"),
                "--daemon requires --endpoint"
            );
            let endpoint = args
                .next()
                .map(std::path::PathBuf::from)
                .ok_or_else(|| anyhow!("--daemon requires an endpoint path"))?;
            anyhow::ensure!(args.next().is_none(), "unexpected daemon arguments");
            crate::daemon::run(endpoint)
        }
        Some("--doctor") => {
            let config = MemoryConfig::discover()?;
            let engine = MemoryEngine::open(config)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&engine.doctor(&DoctorRequest { deep: true })?)?
            );
            Ok(())
        }
        Some("--warmup") => {
            let config = MemoryConfig::discover()?;
            let engine = MemoryEngine::open(config)?;
            println!("{}", serde_json::to_string_pretty(&engine.status()?)?);
            Ok(())
        }
        Some(argument) => Err(anyhow!("unknown argument: {argument}")),
        None => run_protocol(MemoryConfig::discover()?),
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
        Method, ProjectRequest, ProjectResponse, RPC_PROTOCOL_VERSION, Request, Service,
        decode_value, encode_value, read_frame, run_protocol_io,
    };
    use crate::model_proto::{
        CancelModelSwitchRequest, GetModelSwitchStatusRequest, ListModelProfilesRequest,
        ModelPreflightDecision, ModelProfileCapability, ModelProfileRole, ModelRequest,
        ModelStatusCode, ModelSwitchAvailability, ModelSwitchExecutionMode,
        ModelSwitchRebuildPolicy, ModelSwitchState, StartModelSwitchRequest, model_request,
        model_response,
    };
    use crate::{EmbeddingConfig, MemoryConfig, RetrievalMode, SearchRequest};

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
    fn search_request_defaults_to_hybrid_for_older_clients() {
        let request: SearchRequest = serde_json::from_value(json!({ "query": "memory" }))
            .expect("search request should deserialize");
        assert_eq!(request.retrieval_mode, RetrievalMode::Hybrid);
    }

    #[test]
    fn memory_project_dispatch_preserves_the_legacy_response() {
        let (_temp, config) = config();
        let mut service = Service::new(config);
        let (response, shutdown) = service
            .handle_project(ProjectRequest::Memory(Request {
                id: 7,
                method: Method::Shutdown as i32,
                params: Some(encode_value(&json!({}), 0).expect("encode params")),
            }))
            .expect("handle shutdown");
        assert!(shutdown);
        let ProjectResponse::Memory(response) = response else {
            panic!("memory request returned a model response")
        };
        assert_eq!(response.id, 7);
        assert!(response.ok);
        assert!(service.engine.is_none());
    }

    #[test]
    fn typed_model_profiles_do_not_initialize_the_memory_engine() {
        let (_temp, config) = config();
        let mut service = Service::new(config);
        let response = service.handle_model(ModelRequest {
            id: 8,
            operation: Some(model_request::Operation::ListProfiles(
                ListModelProfilesRequest {},
            )),
        });
        assert_eq!(response.id, 8);
        assert_eq!(
            response.status.as_ref().map(|status| status.code),
            Some(ModelStatusCode::Ok as i32)
        );
        assert!(service.engine.is_none());
        let Some(model_response::Result::ListProfiles(result)) = response.result else {
            panic!("profile request returned the wrong result")
        };
        assert_eq!(result.active_profile_id, "qwen3-text-4b-q4");
        assert_eq!(result.profiles.len(), 7);
        assert!(
            result.profiles[0]
                .roles
                .contains(&(ModelProfileRole::DefaultForNewProjects as i32))
        );
        assert!(
            result.profiles[0]
                .capabilities
                .contains(&(ModelProfileCapability::Selectable as i32))
        );
    }

    #[test]
    fn engine_setup_failure_is_returned_as_a_domain_failure() {
        let (temp, config) = config();
        let config = config.with_embedding(EmbeddingConfig {
            model_path: Some(temp.path().join("missing.gguf")),
            ..EmbeddingConfig::default()
        });
        let mut service = Service::new(config);
        let request = ProjectRequest::Memory(Request {
            id: 19,
            method: Method::Status as i32,
            params: Some(encode_value(&json!({}), 0).expect("encode params")),
        });

        let error = service
            .prepare_project_request(&request)
            .expect_err("missing local model should fail setup");
        let ProjectResponse::Memory(response) = Service::setup_failure_response(&request, &error)
        else {
            panic!("memory setup failure returned the wrong domain")
        };
        assert_eq!(response.id, 19);
        assert!(!response.ok);
        assert!(
            response
                .error
                .contains("memory engine initialization failed")
        );
    }

    #[test]
    fn typed_model_preflight_does_not_initialize_the_memory_engine() {
        let (_temp, config) = config();
        let mut service = Service::new(config);
        let response = service.handle_model(ModelRequest {
            id: 9,
            operation: Some(model_request::Operation::StartSwitch(
                StartModelSwitchRequest {
                    switch_id: None,
                    target_profile_id: "qwen3-text-0.6b-q8".to_string(),
                    expected_active_profile_id: None,
                    availability: ModelSwitchAvailability::KeepOldDense as i32,
                    execution_mode: ModelSwitchExecutionMode::DryRun as i32,
                    rebuild_policy: ModelSwitchRebuildPolicy::RejectActiveProfile as i32,
                },
            )),
        });
        assert_eq!(
            response.status.as_ref().map(|status| status.code),
            Some(ModelStatusCode::Ok as i32)
        );
        assert!(service.engine.is_none());
        let Some(model_response::Result::StartSwitch(result)) = response.result else {
            panic!("preflight request returned the wrong result")
        };
        assert_eq!(result.state, ModelSwitchState::Preflight as i32);
        assert_eq!(
            result.preflight.as_ref().map(|value| value.decision),
            Some(ModelPreflightDecision::Blocked as i32)
        );
    }

    #[test]
    fn unavailable_model_operations_return_typed_statuses() {
        let (_temp, config) = config();
        let mut service = Service::new(config);
        let requests = [
            ModelRequest {
                id: 10,
                operation: Some(model_request::Operation::StartSwitch(
                    StartModelSwitchRequest {
                        switch_id: Some("switch-a".to_string()),
                        target_profile_id: "qwen3-text-0.6b-q8".to_string(),
                        expected_active_profile_id: None,
                        availability: ModelSwitchAvailability::KeepOldDense as i32,
                        execution_mode: ModelSwitchExecutionMode::Apply as i32,
                        rebuild_policy: ModelSwitchRebuildPolicy::RejectActiveProfile as i32,
                    },
                )),
            },
            ModelRequest {
                id: 11,
                operation: Some(model_request::Operation::GetSwitchStatus(
                    GetModelSwitchStatusRequest {
                        switch_id: "switch-a".to_string(),
                    },
                )),
            },
            ModelRequest {
                id: 12,
                operation: Some(model_request::Operation::CancelSwitch(
                    CancelModelSwitchRequest {
                        switch_id: "switch-a".to_string(),
                    },
                )),
            },
        ];

        for request in requests {
            let expected_id = request.id;
            let response = service.handle_model(request);
            assert_eq!(response.id, expected_id);
            assert_eq!(
                response.status.as_ref().map(|status| status.code),
                Some(ModelStatusCode::Unavailable as i32)
            );
            assert!(response.result.is_none());
        }
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
