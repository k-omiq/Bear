"""Provisioning controller and request orchestrator.

Coordinates:
- Request parsing and protocol validation.
- Driver dispatching (Modal vs Beam).
- Resumable staged execution with journal persistence at each milestone.
- Approved plan enforcement (plan hash verification).
- IC-2 progress records on stderr and the IC-1 result of apply and resume.
- Ownership-verified cleanup of exactly what the journal recorded.

Which ops report which IC-2 steps:
- inspect: inspect (it also lists the installations already in the account; see
  BaseProviderDriver.discover)
- plan: inspect, validate
- apply and resume: inspect, validate, then volume, state, secret, image, deploy,
  weights, token, endpoint, health in that order. A step the provider does not have is
  reported skip (Modal: secret; Beam: token), and so is a step a previous run finished.
  Modal repeats token and health on every run; Beam repeats secret and health.
- cleanup_plan: validate
- cleanup_apply: validate, inspect, cleanup (pct per deleted resource)
- probe_compatibility: health
- forget_credential: none
"""

from pathlib import Path
import time
from typing import IO, Any, Callable, Dict, List, Optional, Tuple, Union

from provisioner.driver_base import (
    BaseProviderDriver,
    CleanupPlan,
    CompatibilityValidationResult,
    Deadline,
    StepContext,
    compute_canonical_hash,
    production_model,
    utc_now,
)
from provisioner.endpoint import parse_runtime_credential, validate_endpoint_url
from provisioner.journal import (
    STAGE_CLEANED_UP,
    STAGE_CLEANUP_PLANNED,
    STAGE_COMPLETED,
    STAGE_CREDENTIAL_CREATED,
    STAGE_DEPLOYED,
    STAGE_DEPLOYING,
    STAGE_DISCOVERED,
    STAGE_FAILED,
    STAGE_SEEDED,
    STAGE_SEEDING,
    STAGE_VALIDATED,
    InstallationJournal,
    transition_allowed,
)
from provisioner.progress import Progress
from provisioner.protocol import (
    ERR_EXECUTION_FAILED,
    ERR_EXECUTION_TIMEOUT,
    ERR_PROVIDER_UNAVAILABLE,
    ERR_UNAPPROVED_PLAN,
    ERR_UNSUPPORTED_OP,
    ERR_UNSUPPORTED_PROVIDER,
    ERR_VALIDATION,
    HelperRequest,
    HelperResponse,
    OP_APPLY,
    OP_CLEANUP_APPLY,
    OP_CLEANUP_PLAN,
    OP_FORGET_CREDENTIAL,
    OP_INSPECT,
    OP_PLAN,
    OP_PROBE_COMPATIBILITY,
    OP_RESUME,
    PROVIDER_BEAM,
    PROVIDER_MODAL,
    ProtocolError,
    make_error_response,
    make_success_response,
    parse_request,
    validate_hash,
    validate_identifier,
)
from provisioner.redaction import GLOBAL_REGISTRY, is_sensitive_key, redact_string

# The desktop kills apply, resume and cleanup_apply after 30 minutes and every other
# op after 60 seconds; the helper stops itself with a typed error before that.
OP_BUDGET_SECONDS: Dict[str, float] = {
    OP_INSPECT: 50.0,
    OP_PLAN: 50.0,
    OP_APPLY: 27 * 60.0,
    OP_RESUME: 27 * 60.0,
    OP_CLEANUP_PLAN: 50.0,
    OP_CLEANUP_APPLY: 25 * 60.0,
    OP_FORGET_CREDENTIAL: 50.0,
    OP_PROBE_COMPATIBILITY: 50.0,
}
# A cleanup that stopped on one of these is finished by running cleanup again.
RETRY_CLEANUP_CODES = (ERR_EXECUTION_FAILED, ERR_EXECUTION_TIMEOUT, ERR_PROVIDER_UNAVAILABLE)

PIPELINE_STEPS = ("volume", "state", "secret", "image", "deploy", "weights", "token", "endpoint", "health")
# (stage while the step runs, stage once it finished)
STEP_STAGES: Dict[str, Tuple[str, str]] = {
    "volume": (STAGE_DEPLOYING, STAGE_DEPLOYING),
    "state": (STAGE_DEPLOYING, STAGE_DEPLOYING),
    "secret": (STAGE_DEPLOYING, STAGE_DEPLOYING),
    "image": (STAGE_DEPLOYING, STAGE_DEPLOYING),
    "deploy": (STAGE_DEPLOYING, STAGE_DEPLOYED),
    "weights": (STAGE_SEEDING, STAGE_SEEDED),
    "token": (STAGE_CREDENTIAL_CREATED, STAGE_CREDENTIAL_CREATED),
    "endpoint": (STAGE_DISCOVERED, STAGE_DISCOVERED),
    "health": (STAGE_VALIDATED, STAGE_VALIDATED),
}


def _default_drivers() -> Tuple[BaseProviderDriver, BaseProviderDriver]:
    from provisioner.beam_driver import BeamDriver
    from provisioner.modal_driver import ModalDriver

    return ModalDriver(), BeamDriver()


class ProvisioningController:
    """Orchestrates helper protocol requests and cloud driver operations."""

    def __init__(
        self,
        journal_root: Path,
        modal_driver: Optional[BaseProviderDriver] = None,
        beam_driver: Optional[BaseProviderDriver] = None,
        progress_stream: Optional[IO[str]] = None,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self.journal_root = journal_root.resolve()
        if modal_driver is None or beam_driver is None:
            default_modal, default_beam = _default_drivers()
            modal_driver = modal_driver or default_modal
            beam_driver = beam_driver or default_beam
        self.modal_driver = modal_driver
        self.beam_driver = beam_driver
        self.progress_stream = progress_stream
        self.clock = clock

    def get_driver(self, provider: str) -> BaseProviderDriver:
        if provider == PROVIDER_MODAL:
            return self.modal_driver
        if provider == PROVIDER_BEAM:
            return self.beam_driver
        raise ProtocolError(ERR_UNSUPPORTED_PROVIDER, f"Unknown provider: '{provider}'")

    # ---------- envelope ----------

    def handle_request(self, raw_request: Union[str, bytes, Dict[str, Any]]) -> str:
        """Entry point for JSON/dict helper protocol messages. Returns serialized redacted response."""
        request_id = "req-unknown"
        try:
            req = parse_request(raw_request)
            request_id = req.request_id

            # One request is one redaction session (the helper answers one request per
            # process): only this request's credentials are redacted, so an id from an
            # earlier request, like a proxy token ID, is never scrubbed from the journal.
            GLOBAL_REGISTRY.clear()
            self._register_credentials_from_params(req.params)

            handler = {
                OP_INSPECT: self._handle_inspect,
                OP_PLAN: self._handle_plan,
                OP_APPLY: self._handle_apply,
                OP_RESUME: self._handle_resume,
                OP_FORGET_CREDENTIAL: self._handle_forget_credential,
                OP_CLEANUP_PLAN: self._handle_cleanup_plan,
                OP_CLEANUP_APPLY: self._handle_cleanup_apply,
                OP_PROBE_COMPATIBILITY: self._handle_probe_compatibility,
            }.get(req.op)
            if handler is None:
                raise ProtocolError(ERR_UNSUPPORTED_OP, f"Unsupported operation: '{req.op}'")
            progress = Progress(req.op, self.progress_stream)
            deadline = Deadline(OP_BUDGET_SECONDS[req.op], self.clock)
            return handler(req, progress, deadline).serialize()

        except ProtocolError as e:
            return make_error_response(
                request_id=request_id,
                code=e.code,
                message=e.message,
                actionable_guidance=e.actionable_guidance,
                remedy_steps=e.remedy_steps,
            ).serialize()
        except (Exception, SystemExit) as e:
            # SystemExit: some SDK code calls sys.exit on errors; the helper still answers.
            return make_error_response(
                request_id=request_id,
                code=ERR_EXECUTION_FAILED,
                message=f"Internal provisioner error: {type(e).__name__}: {e}",
            ).serialize()

    def _register_credentials_from_params(self, params: Dict[str, Any]) -> None:
        def _extract(obj: Any, key_name: str = "") -> None:
            if isinstance(obj, str):
                if is_sensitive_key(key_name) or key_name == "credentials":
                    GLOBAL_REGISTRY.register(obj)
            elif isinstance(obj, dict):
                for k, v in obj.items():
                    str_k = str(k)
                    if str_k.lower() == "credentials":
                        _extract_all(v)
                    elif is_sensitive_key(str_k):
                        if isinstance(v, str):
                            GLOBAL_REGISTRY.register(v)
                        else:
                            _extract(v, str_k)
                    else:
                        _extract(v, str_k)
            elif isinstance(obj, (list, tuple, set)):
                for item in obj:
                    _extract(item, key_name)

        def _extract_all(obj: Any) -> None:
            if isinstance(obj, str):
                GLOBAL_REGISTRY.register(obj)
            elif isinstance(obj, dict):
                for v in obj.values():
                    _extract_all(v)
            elif isinstance(obj, (list, tuple, set)):
                for item in obj:
                    _extract_all(item)

        _extract(params)

    # ---------- shared pieces ----------

    @staticmethod
    def _installation_id(req: HelperRequest) -> str:
        installation_id = req.params.get("installation_id")
        if not installation_id:
            raise ProtocolError(ERR_VALIDATION, "Missing parameter: 'installation_id'")
        validate_identifier("installation_id", installation_id)
        return installation_id

    def _journal(self, req: HelperRequest, installation_id: str) -> InstallationJournal:
        return InstallationJournal(self.journal_root, installation_id, req.provider)

    @staticmethod
    def _run(progress: Progress, step: str, action: Callable[[], Any]) -> Any:
        with progress.step(step):
            return action()

    def _inspect(self, driver: BaseProviderDriver, req: HelperRequest, progress: Progress, deadline: Deadline, stack: Any):
        """Validate the setup credential, open the SDK session and read the account."""
        credentials = driver.parse_credentials(req.params.get("credentials"))
        with progress.step("inspect"):
            session = stack.enter_context(driver.session(credentials, deadline))
            inspection = driver.inspect(session)
        return session, inspection

    # ---------- inspect and plan ----------

    def _handle_inspect(self, req: HelperRequest, progress: Progress, deadline: Deadline) -> HelperResponse:
        from contextlib import ExitStack

        driver = self.get_driver(req.provider)
        with ExitStack() as stack:
            session, inspection = self._inspect(driver, req, progress, deadline, stack)
            try:
                found, complete = driver.discover(session, deadline)
            except Exception:
                found, complete = [], False
        data = inspection.to_dict()
        data["existing_installations"] = [self._mark_local(req.provider, inspection.account_id, entry) for entry in found]
        data["existing_installations_complete"] = bool(complete)
        return make_success_response(req.request_id, data)

    def _mark_local(self, provider: str, account_id: str, entry: Dict[str, Any]) -> Dict[str, Any]:
        """Whether this computer's journal can resume the installation, which then takes Resume, not apply."""
        local = False
        installation_id = entry.get("installation_id")
        try:
            validate_identifier("installation_id", installation_id)
            journal = InstallationJournal(self.journal_root, installation_id, provider)
            if journal.exists() and journal.can_resume():
                recorded = journal.get_state("account_id")
                local = not recorded or recorded == account_id
        except Exception:
            local = False
        return {**entry, "on_this_computer": bool(local)}

    def _handle_plan(self, req: HelperRequest, progress: Progress, deadline: Deadline) -> HelperResponse:
        """Read-only: nothing is written locally or in the cloud."""
        from contextlib import ExitStack

        driver = self.get_driver(req.provider)
        installation_id = self._installation_id(req)
        options = driver.normalize_options(req.params.get("options"))
        with ExitStack() as stack:
            _, inspection = self._inspect(driver, req, progress, deadline, stack)
            plan = self._run(progress, "validate", lambda: driver.plan(inspection, installation_id, options))
        return make_success_response(req.request_id, plan.to_dict())

    # ---------- apply and resume ----------

    def _handle_apply(self, req: HelperRequest, progress: Progress, deadline: Deadline) -> HelperResponse:
        from contextlib import ExitStack

        driver = self.get_driver(req.provider)
        installation_id = self._installation_id(req)
        approved_hash = req.params.get("approved_plan_hash")
        if not approved_hash:
            raise ProtocolError(
                ERR_UNAPPROVED_PLAN,
                "Execution rejected: 'approved_plan_hash' parameter is required to proceed with deployment",
                actionable_guidance="Review the proposed deployment plan and supply its exact SHA-256 hash to approve.",
            )
        validate_hash("approved_plan_hash", approved_hash)
        options = driver.normalize_options(req.params.get("options"))
        journal = self._journal(req, installation_id)

        with ExitStack() as stack:
            session, inspection = self._inspect(driver, req, progress, deadline, stack)

            def validate() -> None:
                plan = driver.plan(inspection, installation_id, options)
                if approved_hash.lower() != plan.plan_hash.lower():
                    raise ProtocolError(
                        ERR_UNAPPROVED_PLAN,
                        f"Approved plan hash mismatch: got '{approved_hash}', expected '{plan.plan_hash}'",
                        actionable_guidance="The plan changed since it was approved. Review and approve it again.",
                    )
                if journal.exists():
                    rec = journal.load()
                    if rec.stage in {STAGE_COMPLETED, STAGE_CLEANUP_PLANNED, STAGE_CLEANED_UP}:
                        raise ProtocolError(
                            ERR_VALIDATION,
                            f"Cannot apply: installation '{installation_id}' is already in stage '{rec.stage}'",
                            actionable_guidance="Use Resume for a finished installation, or start a new one with a new installation ID.",
                            remedy_steps=[
                                f"Installation '{installation_id}' is in stage '{rec.stage}'",
                                "Supply a new unique installation_id in params to deploy a new instance",
                            ],
                        )
                    if rec.plan_hash.lower() != plan.plan_hash.lower() or (
                        rec.approved_plan_hash and rec.approved_plan_hash.lower() != approved_hash.lower()
                    ):
                        raise ProtocolError(
                            ERR_UNAPPROVED_PLAN,
                            f"Plan hash mismatch with existing journal: stored '{rec.plan_hash}', expected '{plan.plan_hash}'",
                            actionable_guidance="A different plan is already recorded for this installation. Resume it, or use a new installation ID.",
                        )
                    self._record_account(journal, inspection)
                else:
                    journal.initialize(
                        plan.plan_hash, approved_hash, plan.app_name, options, provider_state=self._account_state(inspection)
                    )

            self._run(progress, "validate", validate)
            return self._pipeline(driver, session, journal, req, progress, deadline)

    def _handle_resume(self, req: HelperRequest, progress: Progress, deadline: Deadline) -> HelperResponse:
        from contextlib import ExitStack

        driver = self.get_driver(req.provider)
        installation_id = self._installation_id(req)
        journal = self._journal(req, installation_id)
        if not journal.exists():
            raise ProtocolError(ERR_VALIDATION, f"No existing installation journal found for '{installation_id}' to resume")
        rec = journal.load()
        if not journal.can_resume():
            raise ProtocolError(
                ERR_VALIDATION,
                f"Cannot resume installation '{installation_id}' in stage '{rec.stage}'",
                actionable_guidance="Cleanup has started for this installation. Finish the cleanup instead.",
            )
        if not rec.approved_plan_hash:
            raise ProtocolError(
                ERR_UNAPPROVED_PLAN,
                f"Cannot resume installation '{installation_id}': no approved plan hash recorded in journal",
                actionable_guidance="Approve the deployment plan via apply operation before attempting resume.",
            )
        # The approved options are the journal's. A request may repeat them, or change
        # them with the hash of a plan made for the new options: an update, such as
        # adding page denoise to a finished installation. Nothing else changes them.
        options = driver.normalize_options(rec.options)
        requested = req.params.get("options")
        updated: Optional[Dict[str, Any]] = None
        update_hash = req.params.get("approved_plan_hash")
        if requested and driver.normalize_options(requested) != options:
            if not update_hash:
                raise ProtocolError(
                    ERR_UNAPPROVED_PLAN,
                    "Plan drift detected on resume: the options differ from the approved plan",
                    actionable_guidance="To change the options, plan with the new options and approve that plan's hash.",
                )
            validate_hash("approved_plan_hash", update_hash)
            updated = driver.normalize_options(requested)

        with ExitStack() as stack:
            session, inspection = self._inspect(driver, req, progress, deadline, stack)

            def validate() -> None:
                # The recorded plan regenerates unless the account differs or this
                # release deploys the same choices with another recipe or weights
                # revision. The journal's recorded account tells the two apart, so a
                # new release updates the installation in place. A journal written
                # without the account has only the hash to prove it.
                plan = driver.plan(inspection, installation_id, options)
                for stored, what in ((rec.plan_hash, "stored plan hash"), (rec.approved_plan_hash, "approved plan hash")):
                    if plan.plan_hash.lower() != stored.lower() and not self._same_account(journal, inspection):
                        raise ProtocolError(
                            ERR_UNAPPROVED_PLAN,
                            f"Plan drift detected on resume: regenerated plan hash '{plan.plan_hash}' does not match {what} '{stored}'",
                            actionable_guidance="These credentials do not match the account this setup was made in. Use the same account.",
                        )
                if updated is None and plan.plan_hash.lower() != rec.plan_hash.lower():
                    journal.replan(plan.plan_hash, plan.plan_hash, options, forget_state=driver.seed_state_keys)
                if updated is not None:
                    fresh = driver.plan(inspection, installation_id, updated)
                    if fresh.plan_hash.lower() != str(update_hash).lower():
                        raise ProtocolError(
                            ERR_UNAPPROVED_PLAN,
                            f"Approved plan hash mismatch: got '{update_hash}', expected '{fresh.plan_hash}'",
                            actionable_guidance="The plan changed since it was approved. Review and approve it again.",
                        )
                    journal.replan(fresh.plan_hash, str(update_hash), updated, forget_state=driver.seed_state_keys)
                self._record_account(journal, inspection)

            self._run(progress, "validate", validate)
            return self._pipeline(driver, session, journal, req, progress, deadline)

    @staticmethod
    def _account_state(inspection: Any) -> Dict[str, str]:
        state = {"account_id": inspection.account_id, "environment_name": inspection.environment_name}
        return {key: value for key, value in state.items() if value}

    def _same_account(self, journal: InstallationJournal, inspection: Any) -> bool:
        """Whether these credentials reach the account and environment the journal recorded."""
        recorded = {key: journal.get_state(key) for key in ("account_id", "environment_name") if journal.get_state(key)}
        return bool(recorded.get("account_id")) and recorded == self._account_state(inspection)

    def _record_account(self, journal: InstallationJournal, inspection: Any) -> None:
        # The approved plan hash covers the account and the environment, so once it
        # matched they are the installation's; a journal written without them gets them.
        missing = {key: value for key, value in self._account_state(inspection).items() if not journal.get_state(key)}
        if missing:
            journal.set_state(**missing)

    def _pipeline(
        self,
        driver: BaseProviderDriver,
        session: Any,
        journal: InstallationJournal,
        req: HelperRequest,
        progress: Progress,
        deadline: Deadline,
    ) -> HelperResponse:
        rec = journal.record
        ctx = StepContext(journal=journal, options=driver.normalize_options(rec.options), deadline=deadline)
        try:
            for step in PIPELINE_STEPS:
                if step not in driver.pipeline_steps or (journal.step_done(step) and step not in driver.repeat_steps):
                    progress.skip(step)
                    continue
                running, finished = STEP_STAGES[step]
                journal.advance(running)
                with progress.step(step) as reporter:
                    ctx.reporter = reporter
                    driver.run_step(step, session, ctx)
                if step not in driver.repeat_steps:
                    journal.mark_step_done(step)
                journal.advance(finished)
            credential = driver.issued_credential(session, ctx)
            endpoint_url = validate_endpoint_url(rec.endpoint_url)
            journal.transition_to(STAGE_COMPLETED)
        except (ProtocolError, Exception, SystemExit) as exc:
            message = exc.message if isinstance(exc, ProtocolError) else f"{type(exc).__name__}: {exc}"
            if transition_allowed(journal.record.stage, STAGE_FAILED):
                journal.transition_to(STAGE_FAILED, error=redact_string(message)[:1000])
            raise

        if req.params.get("forget_setup_credential", False) is True:
            journal.forget_setup_credential()
        model = production_model(ctx.options["model_id"])
        data = {
            "installation_id": rec.installation_id,
            "provider": rec.provider,
            "stage": rec.stage,
            "gpu": ctx.options["gpu"],
            "idle_seconds": ctx.options["idle_seconds"],
            "analysis_models": ctx.options["analysis_models"],
            "denoise": bool(ctx.options.get("denoise", False)),
            "model": model,
            "compatibility_status": rec.compatibility_status,
            "resources_created": [{"type": r.resource_type, "name": r.name} for r in rec.resources],
            "setup_credential_forgotten": rec.setup_credential_forgotten,
        }
        # IC-1: the runtime credential and the endpoint travel unredacted, only here.
        return make_success_response(
            req.request_id,
            data,
            verbatim={"runtime_credential": credential, "endpoint_url": endpoint_url},
        )

    # ---------- forget ----------

    def _handle_forget_credential(self, req: HelperRequest, progress: Progress, deadline: Deadline) -> HelperResponse:
        """Record that the desktop dropped the setup credential. The helper never stored it."""
        installation_id = req.params.get("installation_id")
        if installation_id:
            validate_identifier("installation_id", installation_id)
            journal = self._journal(req, installation_id)
            if journal.exists():
                journal.forget_setup_credential()
        return make_success_response(req.request_id, {"status": "forgotten", "setup_credential_cleared": True})

    # ---------- cleanup ----------

    def _cleanup_plan(self, driver: BaseProviderDriver, journal: InstallationJournal, installation_id: str, provider: str) -> Tuple[CleanupPlan, List[Any]]:
        resources = list(journal.record.resources) if journal.exists() else []
        order = {kind: index for index, kind in enumerate(driver.cleanup_order)}
        resources.sort(key=lambda r: (order.get(r.resource_type, len(order)), r.name))
        listed = [
            {"resource_type": r.resource_type, "type": r.resource_type, "name": r.name, "resource_id": r.resource_id}
            for r in resources
        ]
        identity = {"installation_id": installation_id, "provider": provider, "resources": listed}
        plan_hash = compute_canonical_hash(identity, set())
        plan = CleanupPlan(
            plan_id=f"cleanup-{plan_hash[:16]}",
            installation_id=installation_id,
            provider=provider,
            resources_to_delete=listed,
            # Only what this installation recorded is ever deleted; nothing else is looked at.
            foreign_resources_ignored=[],
            persistent_storage_requires_explicit_confirmation=any(r.resource_type == "volume" for r in resources),
            plan_hash=plan_hash,
            created_at_utc=utc_now(),
            notes=(["This plan deletes only proxy tokens recorded on this computer. Tokens issued on other computers for an adopted installation may remain active; review Proxy Auth Tokens in Modal Settings and revoke those you recognize separately."]
                   if provider == "modal" else []),
        )
        return plan, resources

    def _handle_cleanup_plan(self, req: HelperRequest, progress: Progress, deadline: Deadline) -> HelperResponse:
        """Read-only: lists what cleanup_apply would delete. Needs no credentials."""
        driver = self.get_driver(req.provider)
        installation_id = self._installation_id(req)
        journal = self._journal(req, installation_id)
        plan, _ = self._run(progress, "validate", lambda: self._cleanup_plan(driver, journal, installation_id, req.provider))
        return make_success_response(req.request_id, plan.to_dict())

    def _handle_cleanup_apply(self, req: HelperRequest, progress: Progress, deadline: Deadline) -> HelperResponse:
        from contextlib import ExitStack

        driver = self.get_driver(req.provider)
        installation_id = self._installation_id(req)
        approved_hash = req.params.get("approved_cleanup_plan_hash")
        if not approved_hash:
            raise ProtocolError(ERR_UNAPPROVED_PLAN, "Cleanup rejected: 'approved_cleanup_plan_hash' is required to execute deletion")
        validate_hash("approved_cleanup_plan_hash", approved_hash)
        journal = self._journal(req, installation_id)

        def validate() -> Tuple[CleanupPlan, List[Any]]:
            plan, resources = self._cleanup_plan(driver, journal, installation_id, req.provider)
            if approved_hash.lower() != plan.plan_hash.lower():
                raise ProtocolError(
                    ERR_UNAPPROVED_PLAN,
                    f"Approved cleanup plan hash mismatch: got '{approved_hash}', expected '{plan.plan_hash}'",
                )
            if plan.persistent_storage_requires_explicit_confirmation and req.params.get("confirm_delete_persistent_storage") is not True:
                raise ProtocolError(
                    ERR_VALIDATION,
                    "Cleanup rejected: deletion of persistent storage volume requires explicit boolean parameter: 'confirm_delete_persistent_storage=true'",
                    actionable_guidance="Set 'confirm_delete_persistent_storage': true in params to confirm volume deletion.",
                    remedy_steps=[
                        "Confirm that stored model data and cache can be permanently destroyed.",
                        "Resubmit cleanup_apply request with 'confirm_delete_persistent_storage': true.",
                    ],
                )
            return plan, resources

        plan, resources = self._run(progress, "validate", validate)
        deleted: List[Dict[str, str]] = []
        missing: List[Dict[str, str]] = []
        if not resources:
            if journal.exists() and journal.record.stage != STAGE_CLEANED_UP:
                journal.transition_to(STAGE_CLEANUP_PLANNED)
                journal.transition_to(STAGE_CLEANED_UP)
            progress.skip("inspect")
            progress.skip("cleanup")
            return self._cleanup_result(req, installation_id, journal, deleted, missing)

        with ExitStack() as stack:
            session, inspection = self._inspect(driver, req, progress, deadline, stack)
            recorded_account = journal.get_state("account_id")
            if not recorded_account:
                # Without the account, a key for another one would find nothing to delete
                # and the journal would forget resources that still cost money.
                raise ProtocolError(
                    ERR_UNAPPROVED_PLAN,
                    "The installation journal does not record which account it was set up in",
                    actionable_guidance="Run Resume once with the key of that account; it records the account. Then clean up.",
                )
            if recorded_account != inspection.account_id:
                raise ProtocolError(
                    ERR_UNAPPROVED_PLAN,
                    "These credentials belong to a different account than the installation",
                    actionable_guidance="Use a key for the account the installation was set up in.",
                )
            # From here on the installation is being taken apart: never resumed again.
            journal.transition_to(STAGE_CLEANUP_PLANNED)
            with progress.step("cleanup") as reporter:
                try:
                    for index, resource in enumerate(resources):
                        outcome = driver.delete_resource(session, journal, resource, deadline)
                        (deleted if outcome == "deleted" else missing).append(
                            {"type": resource.resource_type, "name": resource.name}
                        )
                        journal.remove_resource(resource.resource_type, resource.name)
                        reporter.pct((index + 1) * 100 // len(resources))
                except (ProtocolError, Exception, SystemExit) as exc:
                    message = exc.message if isinstance(exc, ProtocolError) else f"{type(exc).__name__}: {exc}"
                    journal.record.last_error = redact_string(message)[:1000]
                    journal.save()
                    if isinstance(exc, ProtocolError) and exc.code in RETRY_CLEANUP_CODES:
                        # Driver guidance speaks of Resume, which cleanup has ruled out.
                        exc.actionable_guidance = "Run cleanup again; what was already deleted is skipped."
                    raise
            journal.transition_to(STAGE_CLEANED_UP)
        return self._cleanup_result(req, installation_id, journal, deleted, missing)

    @staticmethod
    def _cleanup_result(
        req: HelperRequest, installation_id: str, journal: InstallationJournal, deleted: List[Any], missing: List[Any]
    ) -> HelperResponse:
        return make_success_response(
            req.request_id,
            {
                "installation_id": installation_id,
                "provider": req.provider,
                "stage": journal.record.stage if journal.exists() else STAGE_CLEANED_UP,
                "deleted": deleted,
                "missing": missing,
            },
        )

    # ---------- probe ----------

    def _handle_probe_compatibility(self, req: HelperRequest, progress: Progress, deadline: Deadline) -> HelperResponse:
        driver = self.get_driver(req.provider)
        endpoint_url = validate_endpoint_url(req.params.get("endpoint_url"))
        credential = parse_runtime_credential(req.params.get("runtime_credential"))
        expected_kind = "modal_proxy" if req.provider == PROVIDER_MODAL else "beam_bearer"
        if credential["kind"] != expected_kind:
            raise ProtocolError(ERR_VALIDATION, f"A {req.provider} endpoint needs a '{expected_kind}' runtime credential")
        report = self._run(progress, "health", lambda: driver.probe(endpoint_url, credential, deadline))
        model = report.get("model", {})
        result = CompatibilityValidationResult(
            endpoint_url=endpoint_url,
            status=report.get("status", "compatible"),
            api_version=str(report.get("protocol_version", "")),
            model_recipe=str(model.get("recipe_id", "")),
            healthy=bool(report.get("ok")),
            gpu_incurred=False,
            latency_ms=float(report.get("latency_ms") or 0.0),
            details={"model": model, "http_status": report.get("http_status")},
        )
        return make_success_response(req.request_id, result.to_dict())
