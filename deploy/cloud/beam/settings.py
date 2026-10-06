"""Names and knobs of one Beam installation, carried in environment variables.

The provisioner builds these settings, exports them with `to_env` before it imports
the staged Beam app module, and every deployment receives the same variables through
its `env`. A container importing the app therefore rebuilds exactly the decorators
that were deployed. This module does not import beam, so the provisioner can plan
without the SDK.
"""

from __future__ import annotations

import os
import re
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Dict, Mapping, Optional

from deploy.cloud.common.deployment import (
    DEFAULT_IDLE_SECONDS,
    DEFAULT_INSTALLATION_ID,
    ENV_INSTALLATION_ID,
    parse_gpu,
    parse_idle_seconds,
)
from deploy.cloud.common.manifest import MODEL_PROD_FLUX, production_model
from deploy.cloud.common.analysis_seed import normalize_analysis_models

GPU_ALLOWLIST = ("RTX4090", "A10G", "RTX5090")
DEFAULT_GPU = "RTX4090"

ENV_PREFIX = "MC_BEAM_PREFIX"
ENV_VOLUME_NAME = "MC_BEAM_VOLUME"
ENV_MAP_NAME = "MC_BEAM_MAP"
ENV_SECRET_NAME = "MC_BEAM_SECRET"
ENV_GPU = "MC_BEAM_GPU"
ENV_IDLE_SECONDS = "MC_BEAM_IDLE_SECONDS"
ENV_MODEL_ID = "MC_BEAM_MODEL_ID"
ENV_ANALYSIS_MODELS = "MC_BEAM_ANALYSIS_MODELS"
ENV_WORKER_URL = "MC_BEAM_WORKER_URL"
ENV_ANALYSIS_URL = "MC_BEAM_ANALYSIS_URL"
ENV_GATEWAY_HOST = "MC_BEAM_GATEWAY_HOST"
ENV_GATEWAY_PORT = "MC_BEAM_GATEWAY_PORT"

DEFAULT_GATEWAY_HOST = "gateway.beam.cloud"
DEFAULT_GATEWAY_PORT = 443

# The provisioner copies deploy/cloud/beam/app.py to the root of a clean staging
# directory under this module name. Beam derives the container handler from the
# module path relative to the working directory, so the module must sit at the root.
APP_MODULE = "mc_beam_app"
SEED_HANDLER = "seed"
WORKER_HANDLER = "render"
GATEWAY_HANDLER = "gateway"

# Beam mounts a volume at mount_path and also exposes it at ./<name> and /volumes/<name>.
WEIGHTS_MOUNT = "./weights"
_ENV_VALUE = re.compile(r"^[^=\s]+$")


@dataclass(frozen=True)
class BeamSettings:
    installation_id: str
    prefix: str
    volume_name: str
    map_name: str
    secret_name: str
    gpu: str
    idle_seconds: int
    model_id: str = MODEL_PROD_FLUX
    analysis_models: tuple[str, ...] = ()
    # Filled in after the worker is deployed; the gateway enqueues jobs there.
    worker_url: str = ""
    analysis_url: str = ""
    gateway_host: str = DEFAULT_GATEWAY_HOST
    gateway_port: int = DEFAULT_GATEWAY_PORT

    @classmethod
    def for_installation(
        cls,
        installation_id: str,
        prefix: str,
        gpu: str = DEFAULT_GPU,
        idle_seconds: int = DEFAULT_IDLE_SECONDS,
        model_id: str = MODEL_PROD_FLUX,
        analysis_models: tuple[str, ...] = (),
        gateway_host: str = DEFAULT_GATEWAY_HOST,
        gateway_port: int = DEFAULT_GATEWAY_PORT,
    ) -> "BeamSettings":
        return cls(
            installation_id=installation_id,
            prefix=prefix,
            volume_name=f"{prefix}-weights",
            map_name=f"{prefix}-jobs",
            secret_name=secret_name_for(prefix),
            gpu=parse_gpu(gpu, GPU_ALLOWLIST),
            idle_seconds=parse_idle_seconds(idle_seconds),
            model_id=production_model(model_id).model_id,
            analysis_models=normalize_analysis_models(analysis_models),
            gateway_host=gateway_host,
            gateway_port=int(gateway_port),
        )

    @classmethod
    def from_env(cls, environ: Optional[Mapping[str, str]] = None) -> "BeamSettings":
        env = os.environ if environ is None else environ
        installation_id = env.get(ENV_INSTALLATION_ID) or DEFAULT_INSTALLATION_ID
        prefix = env.get(ENV_PREFIX) or installation_id
        return cls(
            installation_id=installation_id,
            prefix=prefix,
            volume_name=env.get(ENV_VOLUME_NAME) or f"{prefix}-weights",
            map_name=env.get(ENV_MAP_NAME) or f"{prefix}-jobs",
            secret_name=env.get(ENV_SECRET_NAME) or secret_name_for(prefix),
            gpu=parse_gpu(env.get(ENV_GPU) or DEFAULT_GPU, GPU_ALLOWLIST),
            idle_seconds=parse_idle_seconds(env.get(ENV_IDLE_SECONDS) or DEFAULT_IDLE_SECONDS),
            model_id=production_model(env.get(ENV_MODEL_ID) or MODEL_PROD_FLUX).model_id,
            analysis_models=normalize_analysis_models(tuple(filter(None, (env.get(ENV_ANALYSIS_MODELS) or "").split(",")))),
            worker_url=env.get(ENV_WORKER_URL) or "",
            analysis_url=env.get(ENV_ANALYSIS_URL) or "",
            gateway_host=env.get(ENV_GATEWAY_HOST) or DEFAULT_GATEWAY_HOST,
            gateway_port=int(env.get(ENV_GATEWAY_PORT) or DEFAULT_GATEWAY_PORT),
        )

    def with_worker_url(self, worker_url: str) -> "BeamSettings":
        return replace(self, worker_url=worker_url)

    def with_analysis_url(self, analysis_url: str) -> "BeamSettings":
        return replace(self, analysis_url=analysis_url)

    def to_env(self) -> Dict[str, str]:
        """Variables for the deployments. Beam rejects empty values, so unset ones are left out."""
        env = {
            ENV_INSTALLATION_ID: self.installation_id,
            ENV_PREFIX: self.prefix,
            ENV_VOLUME_NAME: self.volume_name,
            ENV_MAP_NAME: self.map_name,
            ENV_SECRET_NAME: self.secret_name,
            ENV_GPU: self.gpu,
            ENV_IDLE_SECONDS: str(self.idle_seconds),
            ENV_MODEL_ID: self.model_id,
            ENV_ANALYSIS_MODELS: ",".join(self.analysis_models),
            ENV_WORKER_URL: self.worker_url,
            ENV_ANALYSIS_URL: self.analysis_url,
            ENV_GATEWAY_HOST: self.gateway_host,
            ENV_GATEWAY_PORT: str(self.gateway_port),
        }
        for key, value in list(env.items()):
            if not value:
                del env[key]
            elif not _ENV_VALUE.match(value):
                raise ValueError(f"{key} cannot be passed to Beam: values must not contain '=' or spaces")
        return env

    @property
    def seed_name(self) -> str:
        return f"{self.prefix}-seed"

    @property
    def worker_name(self) -> str:
        return f"{self.prefix}-worker"

    @property
    def gateway_name(self) -> str:
        return f"{self.prefix}-gateway"

    @property
    def analysis_name(self) -> str:
        return f"{self.prefix}-analysis"

    @property
    def deployment_names(self) -> Dict[str, str]:
        names = {"seed": self.seed_name, "worker": self.worker_name, "gateway": self.gateway_name}
        if self.analysis_models:
            names["analysis"] = self.analysis_name
        return names


def secret_name_for(prefix: str) -> str:
    """Workspace secret holding the token the gateway uses to enqueue GPU jobs.

    Secrets become environment variables in the container, so the name must be a
    valid variable name and must not collide with the user's own secrets.
    """
    return "MC_" + re.sub(r"[^A-Z0-9]+", "_", prefix.upper()).strip("_") + "_TOKEN"


def resolve_weights_root(volume_name: str, cwd: Optional[Path] = None) -> Path:
    """Where the weights volume shows up inside a Beam container.

    The mount path is relative to the working directory; Beam also links the volume
    at /volumes/<name>. The first existing candidate wins.
    """
    base = Path.cwd() if cwd is None else cwd
    candidates = (base / "weights", Path("/mnt/code/weights"), Path("/volumes") / volume_name)
    for candidate in candidates:
        if candidate.is_dir():
            return candidate
    return candidates[0]
