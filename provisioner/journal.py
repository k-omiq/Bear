"""Resumable installation and resource journal for cloud provisioning.

Provides durable, atomic tracking of provisioning progress and allocated cloud resources:
- Atomic file durability (write to temp file, sync, atomic replace).
- Resumable stage transitions without duplicate resource allocation.
- Invariant: Zero raw credentials or secret tokens written to journal files on disk.
- Ownership tagging on all recorded cloud resources for safe verified teardown.

The desktop may kill the helper at any moment (cancel kills the process group), so
the pipeline writes a resource here before it asks the provider to create it, and a
step is marked done only after its side effect finished. A later `resume` then skips
finished steps and repeats unfinished ones, which are all idempotent.
"""

from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import sys
from typing import Any, Dict, List, Optional, Set, Tuple

from provisioner.progress import STEPS
from provisioner.protocol import (
    ALLOWLISTED_PROVIDERS,
    ERR_SECURITY_VIOLATION,
    ERR_VALIDATION,
    HEX_HASH_REGEX,
    IDENTIFIER_REGEX,
    ProtocolError,
    validate_identifier,
)
from provisioner.redaction import is_sensitive_key, redact_data, redact_string

JOURNAL_SCHEMA_VERSION = "1.0.0"
MAX_JOURNAL_BYTES = 256 * 1024  # 256 KiB safety limit

# Stages
STAGE_PLANNED = "planned"
STAGE_SEEDING = "seeding"
STAGE_SEEDED = "seeded"
STAGE_DEPLOYING = "deploying"
STAGE_DEPLOYED = "deployed"
STAGE_DISCOVERED = "discovered"
STAGE_CREDENTIAL_CREATED = "credential_created"
STAGE_VALIDATED = "validated"
STAGE_COMPLETED = "completed"
STAGE_FAILED = "failed"
STAGE_CLEANUP_PLANNED = "cleanup_planned"
STAGE_CLEANED_UP = "cleaned_up"

VALID_STAGES: Set[str] = {
    STAGE_PLANNED,
    STAGE_SEEDING,
    STAGE_SEEDED,
    STAGE_DEPLOYING,
    STAGE_DEPLOYED,
    STAGE_DISCOVERED,
    STAGE_CREDENTIAL_CREATED,
    STAGE_VALIDATED,
    STAGE_COMPLETED,
    STAGE_FAILED,
    STAGE_CLEANUP_PLANNED,
    STAGE_CLEANED_UP,
}

# The order the apply pipeline passes through, which is the IC-2 step order:
# volume..deploy, weights, token, endpoint, health.
PIPELINE_ORDER: Dict[str, int] = {
    STAGE_PLANNED: 0,
    STAGE_DEPLOYING: 1,
    STAGE_DEPLOYED: 2,
    STAGE_SEEDING: 3,
    STAGE_SEEDED: 4,
    STAGE_CREDENTIAL_CREATED: 5,
    STAGE_DISCOVERED: 6,
    STAGE_VALIDATED: 7,
    STAGE_COMPLETED: 8,
}

_STATE_KEY_REGEX = re.compile(r"^[a-z][a-z0-9_]{0,40}$")
_GPU_REGEX = re.compile(r"^[A-Za-z0-9_-]{1,32}$")
MAX_STATE_ENTRIES = 32
MAX_STATE_VALUE_CHARS = 2048

_COUNTER = 0


def _get_utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def transition_allowed(current: str, new: str) -> bool:
    """The stage machine. Only cleanup leaves the pipeline for good."""
    if new == current:
        return True
    if current == STAGE_CLEANED_UP:
        return False
    if current == STAGE_CLEANUP_PLANNED:
        # A cleanup that stopped half way is finished by another cleanup_apply, never
        # resumed: some of what the pipeline built may already be gone.
        return new == STAGE_CLEANED_UP
    if new == STAGE_CLEANUP_PLANNED:
        return True
    if new == STAGE_CLEANED_UP or new == STAGE_PLANNED:
        return False
    if new == STAGE_FAILED:
        return True
    if new == STAGE_COMPLETED:
        # Completed means the health check passed in this run.
        return current == STAGE_VALIDATED
    if current == STAGE_FAILED:
        return new in PIPELINE_ORDER
    if current == STAGE_COMPLETED:
        # resume at completed redeploys (drivers that repeat the deploy steps)
        # and issues a fresh runtime credential.
        return new in (STAGE_DEPLOYING, STAGE_CREDENTIAL_CREATED)
    return PIPELINE_ORDER[new] > PIPELINE_ORDER[current]


def _check_state_value(key: str, value: Any) -> None:
    if not isinstance(key, str) or not _STATE_KEY_REGEX.match(key) or is_sensitive_key(key):
        raise ProtocolError(ERR_VALIDATION, f"Invalid provider_state key in journal: '{key}'")
    if value is None or isinstance(value, (bool, int)):
        return
    if not isinstance(value, str) or len(value) > MAX_STATE_VALUE_CHARS:
        raise ProtocolError(ERR_VALIDATION, f"Invalid provider_state value for '{key}'")
    if any(ord(c) < 32 or ord(c) == 127 for c in value):
        raise ProtocolError(ERR_SECURITY_VIOLATION, f"Control character in provider_state value for '{key}'")


def _check_options(options: Any) -> Dict[str, Any]:
    if not isinstance(options, dict):
        raise ProtocolError(ERR_VALIDATION, "Field 'options' in journal must be a dictionary")
    extra = set(options) - {"gpu", "idle_seconds", "model_id", "analysis_models", "denoise", "routing_region"}
    if extra:
        raise ProtocolError(ERR_VALIDATION, f"Unknown options in journal: {sorted(extra)}")
    gpu = options.get("gpu")
    if gpu is not None and (not isinstance(gpu, str) or not _GPU_REGEX.match(gpu)):
        raise ProtocolError(ERR_VALIDATION, "Invalid 'options.gpu' in journal")
    idle = options.get("idle_seconds")
    if idle is not None and (isinstance(idle, bool) or not isinstance(idle, int) or not 0 < idle <= 86400):
        raise ProtocolError(ERR_VALIDATION, "Invalid 'options.idle_seconds' in journal")
    model_id = options.get("model_id")
    if model_id is not None:
        from deploy.cloud.common.manifest import PRODUCTION_MODELS
        if not isinstance(model_id, str) or model_id not in PRODUCTION_MODELS:
            raise ProtocolError(ERR_VALIDATION, "Invalid 'options.model_id' in journal")
    analysis_models = options.get("analysis_models")
    if analysis_models is not None:
        from deploy.cloud.common.analysis_seed import normalize_analysis_models
        try:
            normalize_analysis_models(analysis_models)
        except ValueError:
            raise ProtocolError(ERR_VALIDATION, "Invalid 'options.analysis_models' in journal") from None
    if "denoise" in options and not isinstance(options["denoise"], bool):
        raise ProtocolError(ERR_VALIDATION, "Invalid 'options.denoise' in journal")
    if "routing_region" in options:
        from deploy.cloud.common.deployment import ROUTING_REGIONS
        if options["routing_region"] not in ROUTING_REGIONS:
            raise ProtocolError(ERR_VALIDATION, "Invalid 'options.routing_region' in journal")
    return dict(options)


@dataclass
class ResourceRecord:
    resource_id: str
    resource_type: str
    provider: str
    name: str
    stage_created: str
    created_at_utc: str
    ownership_tags: Dict[str, str]

    def to_dict(self) -> Dict[str, Any]:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "ResourceRecord":
        if not isinstance(data, dict):
            raise ProtocolError(ERR_VALIDATION, "Resource record must be a JSON object")

        resource_id = data.get("resource_id")
        if not resource_id or not isinstance(resource_id, str):
            raise ProtocolError(ERR_VALIDATION, "Missing or invalid 'resource_id' in resource record")
        validate_identifier("resource_id", resource_id)

        resource_type = data.get("resource_type")
        if not resource_type or not isinstance(resource_type, str):
            raise ProtocolError(ERR_VALIDATION, "Missing or invalid 'resource_type' in resource record")
        validate_identifier("resource_type", resource_type)

        provider = data.get("provider")
        if not provider or not isinstance(provider, str) or provider not in ALLOWLISTED_PROVIDERS:
            raise ProtocolError(ERR_VALIDATION, f"Missing or invalid 'provider' in resource record: '{provider}'")

        name = data.get("name")
        if not name or not isinstance(name, str):
            raise ProtocolError(ERR_VALIDATION, "Missing or invalid 'name' in resource record")
        validate_identifier("name", name)

        stage_created = data.get("stage_created")
        if not stage_created or not isinstance(stage_created, str) or stage_created not in VALID_STAGES:
            raise ProtocolError(ERR_VALIDATION, f"Invalid 'stage_created' in resource record: '{stage_created}'")

        created_at_utc = data.get("created_at_utc")
        if not created_at_utc or not isinstance(created_at_utc, str):
            raise ProtocolError(ERR_VALIDATION, "Missing or invalid 'created_at_utc' in resource record")

        ownership_tags = data.get("ownership_tags")
        if not isinstance(ownership_tags, dict):
            raise ProtocolError(ERR_VALIDATION, "Field 'ownership_tags' in resource record must be a dictionary")

        for k, v in ownership_tags.items():
            if not isinstance(k, str) or not isinstance(v, str):
                raise ProtocolError(ERR_VALIDATION, "Ownership tag keys and values must be strings")
            if "\0" in k or "\0" in v:
                raise ProtocolError(ERR_SECURITY_VIOLATION, "Null byte detected in ownership tags")

        if ownership_tags.get("managed_by") != "manga-cleaner":
            raise ProtocolError(
                ERR_VALIDATION,
                f"Resource record '{resource_id}' missing or invalid managed_by tag: '{ownership_tags.get('managed_by')}'",
            )
        inst_tag = ownership_tags.get("installation_id")
        if not inst_tag or not isinstance(inst_tag, str):
            raise ProtocolError(
                ERR_VALIDATION,
                f"Resource record '{resource_id}' missing or invalid installation_id tag",
            )
        validate_identifier("ownership_tags.installation_id", inst_tag)

        return cls(
            resource_id=resource_id,
            resource_type=resource_type,
            provider=provider,
            name=name,
            stage_created=stage_created,
            created_at_utc=created_at_utc,
            ownership_tags=dict(ownership_tags),
        )


@dataclass
class InstallationRecord:
    schema_version: str
    installation_id: str
    provider: str
    stage: str
    plan_hash: str
    approved_plan_hash: str
    app_name: str
    resources: List[ResourceRecord]
    endpoint_url: Optional[str] = None
    # A pointer to the credential resource, never the credential itself.
    runtime_credential_ref: Optional[Dict[str, Any]] = None
    compatibility_status: Optional[str] = None
    setup_credential_forgotten: bool = False
    created_at_utc: str = field(default_factory=_get_utc_now)
    updated_at_utc: str = field(default_factory=_get_utc_now)
    last_error: Optional[str] = None
    # The approved model, analysis graphs, gpu and idle_seconds; resume uses these.
    options: Dict[str, Any] = field(default_factory=dict)
    # Provider ids a later run needs (environment, call ids, URLs). Never secrets.
    provider_state: Dict[str, Any] = field(default_factory=dict)
    # Pipeline steps whose side effects finished; resume skips them.
    completed_steps: List[str] = field(default_factory=list)

    def to_dict(self) -> Dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "installation_id": self.installation_id,
            "provider": self.provider,
            "stage": self.stage,
            "plan_hash": self.plan_hash,
            "approved_plan_hash": self.approved_plan_hash,
            "app_name": self.app_name,
            "resources": [r.to_dict() for r in self.resources],
            "endpoint_url": self.endpoint_url,
            "runtime_credential_ref": self.runtime_credential_ref,
            "compatibility_status": self.compatibility_status,
            "setup_credential_forgotten": self.setup_credential_forgotten,
            "created_at_utc": self.created_at_utc,
            "updated_at_utc": self.updated_at_utc,
            "last_error": self.last_error,
            "options": dict(self.options),
            "provider_state": dict(self.provider_state),
            "completed_steps": list(self.completed_steps),
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "InstallationRecord":
        if not isinstance(data, dict):
            raise ProtocolError(ERR_VALIDATION, "Journal record must be a JSON object")

        if data.get("schema_version") != JOURNAL_SCHEMA_VERSION:
            raise ProtocolError(
                ERR_VALIDATION,
                f"Unsupported journal schema version: '{data.get('schema_version')}'. Expected '{JOURNAL_SCHEMA_VERSION}'",
            )

        installation_id = data.get("installation_id")
        if not installation_id or not isinstance(installation_id, str):
            raise ProtocolError(ERR_VALIDATION, "Missing or invalid 'installation_id' in journal")
        validate_identifier("installation_id", installation_id)

        provider = data.get("provider")
        if not provider or not isinstance(provider, str) or provider not in ALLOWLISTED_PROVIDERS:
            raise ProtocolError(ERR_VALIDATION, f"Invalid or unsupported provider in journal: '{provider}'")

        stage = data.get("stage")
        if not stage or not isinstance(stage, str) or stage not in VALID_STAGES:
            raise ProtocolError(ERR_VALIDATION, f"Invalid or tampered stage in journal: '{stage}'")

        plan_hash = data.get("plan_hash")
        if not isinstance(plan_hash, str):
            raise ProtocolError(ERR_VALIDATION, "Field 'plan_hash' must be a string")
        if plan_hash != "" and not HEX_HASH_REGEX.match(plan_hash):
            raise ProtocolError(ERR_VALIDATION, f"Invalid 'plan_hash' in journal: '{plan_hash}'")

        approved_plan_hash = data.get("approved_plan_hash")
        if not isinstance(approved_plan_hash, str):
            raise ProtocolError(ERR_VALIDATION, "Field 'approved_plan_hash' must be a string")
        if approved_plan_hash != "" and not HEX_HASH_REGEX.match(approved_plan_hash):
            raise ProtocolError(ERR_VALIDATION, f"Invalid 'approved_plan_hash' in journal: '{approved_plan_hash}'")

        app_name = data.get("app_name")
        if not isinstance(app_name, str):
            raise ProtocolError(ERR_VALIDATION, "Field 'app_name' must be a string")
        if app_name != "" and not IDENTIFIER_REGEX.match(app_name):
            raise ProtocolError(ERR_VALIDATION, f"Invalid 'app_name' in journal: '{app_name}'")

        resources_raw = data.get("resources")
        if not isinstance(resources_raw, list):
            raise ProtocolError(ERR_VALIDATION, "Field 'resources' in journal must be a list")

        resources = []
        for r in resources_raw:
            rec_r = ResourceRecord.from_dict(r)
            if rec_r.provider != provider:
                raise ProtocolError(
                    ERR_VALIDATION,
                    f"Resource record '{rec_r.resource_id}' provider '{rec_r.provider}' does not match journal provider '{provider}'",
                )
            if rec_r.ownership_tags.get("installation_id") != installation_id:
                raise ProtocolError(
                    ERR_VALIDATION,
                    f"Resource record '{rec_r.resource_id}' ownership tag installation_id does not match journal installation_id",
                )
            resources.append(rec_r)

        endpoint_url = data.get("endpoint_url")
        if endpoint_url is not None:
            if not isinstance(endpoint_url, str):
                raise ProtocolError(ERR_VALIDATION, "Field 'endpoint_url' must be a string or null")
            if endpoint_url != "" and not endpoint_url.lower().startswith("https://"):
                raise ProtocolError(ERR_VALIDATION, f"Stored endpoint_url must be HTTPS: '{endpoint_url}'")

        runtime_credential_ref = data.get("runtime_credential_ref")
        if runtime_credential_ref is not None and not isinstance(runtime_credential_ref, dict):
            raise ProtocolError(ERR_VALIDATION, "Field 'runtime_credential_ref' must be a dict or null")

        compatibility_status = data.get("compatibility_status")
        if compatibility_status is not None and not isinstance(compatibility_status, str):
            raise ProtocolError(ERR_VALIDATION, "Field 'compatibility_status' must be a string or null")

        setup_credential_forgotten = data.get("setup_credential_forgotten", False)
        if not isinstance(setup_credential_forgotten, bool):
            raise ProtocolError(ERR_VALIDATION, "Field 'setup_credential_forgotten' must be a boolean")

        created_at_utc = data.get("created_at_utc", _get_utc_now())
        if not isinstance(created_at_utc, str):
            raise ProtocolError(ERR_VALIDATION, "Field 'created_at_utc' must be a string")

        updated_at_utc = data.get("updated_at_utc", _get_utc_now())
        if not isinstance(updated_at_utc, str):
            raise ProtocolError(ERR_VALIDATION, "Field 'updated_at_utc' must be a string")

        last_error = data.get("last_error")
        if last_error is not None and not isinstance(last_error, str):
            raise ProtocolError(ERR_VALIDATION, "Field 'last_error' must be a string or null")

        options = _check_options(data.get("options", {}))

        provider_state = data.get("provider_state", {})
        if not isinstance(provider_state, dict) or len(provider_state) > MAX_STATE_ENTRIES:
            raise ProtocolError(ERR_VALIDATION, "Field 'provider_state' in journal must be a small dictionary")
        for key, value in provider_state.items():
            _check_state_value(key, value)

        completed_steps = data.get("completed_steps", [])
        if (
            not isinstance(completed_steps, list)
            or any(step not in STEPS for step in completed_steps)
            or len(set(completed_steps)) != len(completed_steps)
        ):
            raise ProtocolError(ERR_VALIDATION, "Field 'completed_steps' in journal is invalid")

        return cls(
            schema_version=data["schema_version"],
            installation_id=installation_id,
            provider=provider,
            stage=stage,
            plan_hash=plan_hash,
            approved_plan_hash=approved_plan_hash,
            app_name=app_name,
            resources=resources,
            endpoint_url=endpoint_url,
            runtime_credential_ref=runtime_credential_ref,
            compatibility_status=compatibility_status,
            setup_credential_forgotten=setup_credential_forgotten,
            created_at_utc=created_at_utc,
            updated_at_utc=updated_at_utc,
            last_error=last_error,
            options=options,
            provider_state=dict(provider_state),
            completed_steps=list(completed_steps),
        )


class InstallationJournal:
    """Manages atomic persistence and state machine progression for an installation."""

    def __init__(self, root_dir: Path, installation_id: str, provider: str) -> None:
        validate_identifier("installation_id", installation_id)
        if provider not in ALLOWLISTED_PROVIDERS:
            raise ProtocolError(
                ERR_VALIDATION,
                f"Invalid or unsupported provider: '{provider}'",
            )
        self.root_dir = root_dir.resolve()
        self.installation_id = installation_id
        self.provider = provider
        self.installation_dir = self.root_dir / "installations" / installation_id
        self.journal_file = self.installation_dir / "journal.json"
        self._record: Optional[InstallationRecord] = None

    @property
    def record(self) -> InstallationRecord:
        if self._record is None:
            self._record = self.load()
        return self._record

    def _check_symlink_hazards(self) -> None:
        """Fail closed if installation directory or journal file is a symbolic link."""
        if self.installation_dir.is_symlink() or os.path.islink(str(self.installation_dir)):
            raise ProtocolError(
                ERR_SECURITY_VIOLATION,
                f"Journal directory cannot be a symbolic link: {self.installation_dir}",
            )
        if self.journal_file.is_symlink() or os.path.islink(str(self.journal_file)):
            raise ProtocolError(
                ERR_SECURITY_VIOLATION,
                f"Journal file cannot be a symbolic link: {self.journal_file}",
            )

    def exists(self) -> bool:
        self._check_symlink_hazards()
        return self.journal_file.is_file()

    def load(self) -> InstallationRecord:
        """Load and parse journal from disk with strict byte limits and identity/shape validation."""
        self._check_symlink_hazards()
        if not self.journal_file.is_file():
            raise FileNotFoundError(f"Journal not found at {self.journal_file}")

        file_size = self.journal_file.stat().st_size
        if file_size > MAX_JOURNAL_BYTES:
            raise ProtocolError(
                ERR_SECURITY_VIOLATION,
                f"Journal file {self.journal_file} size ({file_size} bytes) exceeds safety limit {MAX_JOURNAL_BYTES}",
            )

        raw_bytes = self.journal_file.read_bytes()
        try:
            data = json.loads(raw_bytes.decode("utf-8"))
        except Exception as e:
            raise ProtocolError(
                ERR_VALIDATION,
                f"Corrupt journal file at {self.journal_file}: {e}",
            )

        if not isinstance(data, dict):
            raise ProtocolError(
                ERR_VALIDATION,
                f"Corrupt journal file at {self.journal_file}: root is not a dict",
            )

        # Strictly validate stored installation_id and provider equal requested journal identity
        stored_id = data.get("installation_id")
        if stored_id != self.installation_id:
            raise ProtocolError(
                ERR_VALIDATION,
                f"Journal installation_id mismatch: stored '{stored_id}', requested '{self.installation_id}'",
            )

        stored_provider = data.get("provider")
        if stored_provider != self.provider:
            raise ProtocolError(
                ERR_VALIDATION,
                f"Journal provider mismatch: stored '{stored_provider}', requested '{self.provider}'",
            )

        self._record = InstallationRecord.from_dict(data)
        return self._record

    def initialize(
        self,
        plan_hash: str,
        approved_plan_hash: str,
        app_name: str,
        options: Optional[Dict[str, Any]] = None,
        provider_state: Optional[Dict[str, Any]] = None,
    ) -> InstallationRecord:
        """Write a new record at stage planned, with its first provider_state, in one save.

        Refuses to replace an existing journal.
        """
        if self.exists():
            raise ProtocolError(ERR_VALIDATION, f"Installation '{self.installation_id}' already has a journal")
        state = dict(provider_state or {})
        for key, value in state.items():
            _check_state_value(key, value)
        now = _get_utc_now()
        self._record = InstallationRecord(
            schema_version=JOURNAL_SCHEMA_VERSION,
            installation_id=self.installation_id,
            provider=self.provider,
            stage=STAGE_PLANNED,
            plan_hash=plan_hash,
            approved_plan_hash=approved_plan_hash,
            app_name=app_name,
            resources=[],
            created_at_utc=now,
            updated_at_utc=now,
            options=_check_options(dict(options or {})),
            provider_state={key: value for key, value in state.items() if value is not None},
        )
        self.save()
        return self._record

    def replan(
        self,
        plan_hash: str,
        approved_plan_hash: str,
        options: Dict[str, Any],
        forget_state: Tuple[str, ...] = (),
    ) -> InstallationRecord:
        """Record a newly approved plan for this installation, in one save.

        Its options change; its resources and account stay. The image, deployment,
        weights and endpoint steps run again so the new release's seed, worker and
        gateway code serve the updated recipe and choices. New options may need
        models the volume lacks. `forget_state` names the provider state of the
        last download, so the next one is a new download rather than a wait on
        the finished one.
        """
        if self.record.stage in {STAGE_CLEANUP_PLANNED, STAGE_CLEANED_UP}:
            raise ProtocolError(ERR_VALIDATION, f"Installation '{self.installation_id}' is being cleaned up")
        self.record.plan_hash = plan_hash
        self.record.approved_plan_hash = approved_plan_hash
        self.record.options = _check_options(dict(options))
        self.record.completed_steps = [
            step for step in self.record.completed_steps
            if step not in {"image", "deploy", "weights", "endpoint"}
        ]
        for key in forget_state:
            self.record.provider_state.pop(key, None)
        self.save()
        return self.record

    def load_or_initialize(
        self,
        plan_hash: str,
        approved_plan_hash: str,
        app_name: str,
        options: Optional[Dict[str, Any]] = None,
    ) -> InstallationRecord:
        """Load existing journal or initialize a new record."""
        if self.exists():
            return self.load()
        return self.initialize(plan_hash, approved_plan_hash, app_name, options)

    def save(self) -> None:
        """Atomically persist record to disk with fsync and directory sync."""
        self._check_symlink_hazards()
        if self._record is None:
            return

        self._record.updated_at_utc = _get_utc_now()
        self.installation_dir.mkdir(parents=True, exist_ok=True)

        self._check_symlink_hazards()

        # No secret reaches the disk: secret-named keys and every known secret value
        # are redacted throughout. Token-shape guessing runs only over the free-text
        # error, because ids, names and the endpoint URL must round-trip exactly and a
        # user-chosen workspace or environment name can look like a token.
        record_dict = self._record.to_dict()
        sanitized_dict = redact_data(record_dict, patterns=False)
        if isinstance(sanitized_dict.get("last_error"), str):
            sanitized_dict["last_error"] = redact_string(sanitized_dict["last_error"])

        payload = json.dumps(sanitized_dict, indent=2, sort_keys=True).encode("utf-8")
        if len(payload) > MAX_JOURNAL_BYTES:
            raise ProtocolError(
                ERR_SECURITY_VIOLATION,
                f"Serialized journal payload size {len(payload)} exceeds {MAX_JOURNAL_BYTES} bytes",
            )

        # Atomic write pattern: temporary sibling file -> fsync -> rename
        global _COUNTER
        _COUNTER += 1
        tmp_name = f".journal.tmp.{os.getpid()}_{_COUNTER}"
        tmp_path = self.installation_dir / tmp_name

        try:
            self._check_symlink_hazards()
            flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
            if hasattr(os, "O_NOFOLLOW"):
                flags |= os.O_NOFOLLOW
            mode = 0o600
            fd = os.open(str(tmp_path), flags, mode)
            try:
                f = open(fd, "wb", closefd=True)
            except BaseException:
                # Only here is fd still ours: once the file object exists it closes fd,
                # also when a write fails, and the number may already be reused.
                os.close(fd)
                raise
            with f:
                f.write(payload)
                f.flush()
                os.fsync(f.fileno())

            # Atomic replace
            self._check_symlink_hazards()
            os.replace(tmp_path, self.journal_file)

            # Sync parent directory on Unix
            if hasattr(os, "O_DIRECTORY") and sys.platform != "win32":
                try:
                    dir_fd = os.open(str(self.installation_dir), os.O_RDONLY | os.O_DIRECTORY)
                    try:
                        os.fsync(dir_fd)
                    finally:
                        os.close(dir_fd)
                except OSError:
                    pass
        finally:
            if tmp_path.exists():
                try:
                    tmp_path.unlink()
                except OSError:
                    pass

    def transition_to(
        self,
        new_stage: str,
        error: Optional[str] = None,
    ) -> None:
        """Transition state machine to a new stage with strict legal transition validation."""
        if new_stage not in VALID_STAGES:
            raise ProtocolError(
                ERR_VALIDATION,
                f"Invalid state machine stage: '{new_stage}'",
            )
        rec = self.record
        if not transition_allowed(rec.stage, new_stage):
            raise ProtocolError(
                ERR_VALIDATION,
                f"Illegal state transition from '{rec.stage}' to '{new_stage}'",
            )
        rec.stage = new_stage
        rec.last_error = error
        self.save()

    def advance(self, stage: str) -> None:
        """Move the pipeline forward to `stage`; a step repeated on resume leaves it."""
        if transition_allowed(self.record.stage, stage):
            self.transition_to(stage)

    def record_resource(
        self,
        resource_id: str,
        resource_type: str,
        name: str,
        stage_created: str,
        ownership_tags: Optional[Dict[str, str]] = None,
    ) -> ResourceRecord:
        """Record a cloud resource (before creating it, when its name is known in advance)."""
        validate_identifier("resource_id", resource_id)
        validate_identifier("resource_type", resource_type)
        validate_identifier("name", name)

        rec = self.record
        # Prevent duplicate resource entries
        for r in rec.resources:
            if r.resource_id == resource_id or (r.resource_type == resource_type and r.name == name):
                return r

        tags = {
            "installation_id": self.installation_id,
            "managed_by": "manga-cleaner",
            "provider": self.provider,
        }
        if ownership_tags:
            tags.update(ownership_tags)

        new_resource = ResourceRecord(
            resource_id=resource_id,
            resource_type=resource_type,
            provider=self.provider,
            name=name,
            stage_created=stage_created,
            created_at_utc=_get_utc_now(),
            ownership_tags=tags,
        )
        rec.resources.append(new_resource)
        self.save()
        return new_resource

    def remove_resource(self, resource_type: str, name: str) -> None:
        rec = self.record
        kept = [r for r in rec.resources if not (r.resource_type == resource_type and r.name == name)]
        if len(kept) != len(rec.resources):
            rec.resources = kept
            ref = rec.runtime_credential_ref or {}
            if ref.get("resource_type") == resource_type and ref.get("name") == name:
                rec.runtime_credential_ref = None
            self.save()

    def has_resource(self, resource_type: str, name: str) -> bool:
        """Check if resource was already created in this installation."""
        rec = self.record
        return any(r.resource_type == resource_type and r.name == name for r in rec.resources)

    def get_resource(self, resource_type: str, name: Optional[str] = None) -> Optional[ResourceRecord]:
        """Find the first resource of a type (and name, when given)."""
        for r in self.record.resources:
            if r.resource_type == resource_type and (name is None or r.name == name):
                return r
        return None

    def step_done(self, step: str) -> bool:
        return step in self.record.completed_steps

    def mark_step_done(self, step: str) -> None:
        if step not in STEPS:
            raise ProtocolError(ERR_VALIDATION, f"Unknown pipeline step: '{step}'")
        if step not in self.record.completed_steps:
            self.record.completed_steps.append(step)
            self.save()

    def get_state(self, key: str, default: Any = None) -> Any:
        return self.record.provider_state.get(key, default)

    def set_state(self, **values: Any) -> None:
        state = dict(self.record.provider_state)
        for key, value in values.items():
            _check_state_value(key, value)
            if value is None:
                state.pop(key, None)
            else:
                state[key] = value
        if len(state) > MAX_STATE_ENTRIES:
            raise ProtocolError(ERR_VALIDATION, "Too many provider_state entries")
        if state != self.record.provider_state:
            self.record.provider_state = state
            self.save()

    def forget_setup_credential(self) -> None:
        """Record that setup credential was safely wiped from local session."""
        rec = self.record
        rec.setup_credential_forgotten = True
        self.save()

    def can_resume(self) -> bool:
        """Anything short of cleanup can be resumed; at completed, resume re-issues the credential."""
        return self.record.stage not in {STAGE_CLEANUP_PLANNED, STAGE_CLEANED_UP}
