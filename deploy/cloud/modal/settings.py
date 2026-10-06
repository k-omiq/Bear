"""Names and knobs of one Modal installation, carried in environment variables.

The provisioner builds these settings, exports them with `to_env` before it imports
`deploy.cloud.modal.app`, and the app bakes the same variables into every image. A
container importing the app module therefore sees exactly the values that were
deployed. This module does not import modal, so the provisioner can plan without it.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Dict, Mapping, Optional

from deploy.cloud.common.deployment import (
    DEFAULT_IDLE_SECONDS,
    DEFAULT_INSTALLATION_ID,
    DEFAULT_ROUTING_REGION,
    ENV_INSTALLATION_ID,
    parse_gpu,
    parse_idle_seconds,
    parse_routing_region,
)
from deploy.cloud.common.manifest import MODEL_PROD_FLUX, production_model
from deploy.cloud.common.analysis_seed import normalize_analysis_models

GPU_ALLOWLIST = ("L4", "A10", "L40S")
DEFAULT_GPU = "L4"
# USD per hour, Modal list price (https://modal.com/pricing). The provisioner quotes
# it before a deploy and the gateway reports it beside a warm GPU as an estimate.
GPU_LIST_PRICE_USD_PER_HOUR = {"L4": 0.80, "A10": 1.10, "L40S": 1.95}

ENV_APP_NAME = "MC_MODAL_APP_NAME"
ENV_VOLUME_NAME = "MC_MODAL_VOLUME"
ENV_DICT_NAME = "MC_MODAL_DICT"
ENV_ENVIRONMENT = "MC_MODAL_ENVIRONMENT"
ENV_GPU = "MC_MODAL_GPU"
ENV_IDLE_SECONDS = "MC_MODAL_IDLE_SECONDS"
ENV_MODEL_ID = "MC_MODAL_MODEL_ID"
ENV_ANALYSIS_MODELS = "MC_MODAL_ANALYSIS_MODELS"
ENV_DENOISE = "MC_MODAL_DENOISE"
ENV_ROUTING_REGION = "MC_MODAL_ROUTING_REGION"

# Object names inside the app; the provisioner looks these up after deploying.
GATEWAY_FUNCTION = "gateway"
SEED_FUNCTION = "seed_weights"
WORKER_CLASS = "Worker"

WEIGHTS_MOUNT = "/weights"


@dataclass(frozen=True)
class ModalSettings:
    installation_id: str
    app_name: str
    volume_name: str
    dict_name: str
    # Empty means the workspace default environment.
    environment_name: str
    gpu: str
    idle_seconds: int
    model_id: str = MODEL_PROD_FLUX
    analysis_models: tuple[str, ...] = ()
    # Page denoise: seed its models and serve /mc/denoise/v1 on the analysis GPU.
    denoise: bool = False
    # Where Modal's proxy for the gateway sits (deployment.ROUTING_REGIONS).
    routing_region: str = DEFAULT_ROUTING_REGION

    @classmethod
    def for_installation(
        cls,
        installation_id: str,
        prefix: str,
        environment_name: str = "",
        gpu: str = DEFAULT_GPU,
        idle_seconds: int = DEFAULT_IDLE_SECONDS,
        model_id: str = MODEL_PROD_FLUX,
        analysis_models: tuple[str, ...] = (),
        denoise: bool = False,
        routing_region: str = DEFAULT_ROUTING_REGION,
    ) -> "ModalSettings":
        return cls(
            installation_id=installation_id,
            app_name=prefix,
            volume_name=f"{prefix}-weights",
            dict_name=f"{prefix}-jobs",
            environment_name=environment_name,
            gpu=parse_gpu(gpu, GPU_ALLOWLIST),
            idle_seconds=parse_idle_seconds(idle_seconds),
            model_id=production_model(model_id).model_id,
            analysis_models=normalize_analysis_models(analysis_models),
            denoise=bool(denoise),
            routing_region=parse_routing_region(routing_region),
        )

    @classmethod
    def from_env(cls, environ: Optional[Mapping[str, str]] = None) -> "ModalSettings":
        env = os.environ if environ is None else environ
        installation_id = env.get(ENV_INSTALLATION_ID) or DEFAULT_INSTALLATION_ID
        app_name = env.get(ENV_APP_NAME) or installation_id
        return cls(
            installation_id=installation_id,
            app_name=app_name,
            volume_name=env.get(ENV_VOLUME_NAME) or f"{app_name}-weights",
            dict_name=env.get(ENV_DICT_NAME) or f"{app_name}-jobs",
            environment_name=env.get(ENV_ENVIRONMENT) or "",
            gpu=parse_gpu(env.get(ENV_GPU) or DEFAULT_GPU, GPU_ALLOWLIST),
            idle_seconds=parse_idle_seconds(env.get(ENV_IDLE_SECONDS) or DEFAULT_IDLE_SECONDS),
            model_id=production_model(env.get(ENV_MODEL_ID) or MODEL_PROD_FLUX).model_id,
            analysis_models=normalize_analysis_models(tuple(filter(None, (env.get(ENV_ANALYSIS_MODELS) or "").split(",")))),
            denoise=env.get(ENV_DENOISE) == "1",
            routing_region=parse_routing_region(env.get(ENV_ROUTING_REGION) or DEFAULT_ROUTING_REGION),
        )

    def to_env(self) -> Dict[str, str]:
        return {
            ENV_INSTALLATION_ID: self.installation_id,
            ENV_APP_NAME: self.app_name,
            ENV_VOLUME_NAME: self.volume_name,
            ENV_DICT_NAME: self.dict_name,
            ENV_ENVIRONMENT: self.environment_name,
            ENV_GPU: self.gpu,
            ENV_IDLE_SECONDS: str(self.idle_seconds),
            ENV_MODEL_ID: self.model_id,
            ENV_ANALYSIS_MODELS: ",".join(self.analysis_models),
            ENV_DENOISE: "1" if self.denoise else "0",
            ENV_ROUTING_REGION: self.routing_region,
        }

    @property
    def gateway_function(self) -> str:
        """The gateway Function's name in the app.

        Modal fixes a Function's routing region at its first deploy and refuses a
        change on a later one, so each region other than the default has a Function
        of its own name: choosing another region creates a new Function (and a new
        endpoint URL) instead of redeploying the old one.
        """
        if self.routing_region == DEFAULT_ROUTING_REGION:
            return GATEWAY_FUNCTION
        return f"{GATEWAY_FUNCTION}_{self.routing_region.replace('-', '_')}"

    @property
    def environment(self) -> Optional[str]:
        """Environment name for SDK calls; None selects the workspace default."""
        return self.environment_name or None
