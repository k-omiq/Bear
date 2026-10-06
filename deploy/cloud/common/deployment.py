"""Deployment knobs shared by the Modal app, the Beam app and the provisioner.

The provisioner validates a plan with these helpers and passes the result to the
provider app through environment variables; the app validates again when a container
imports it, so a bad value fails the deploy instead of producing a half-working app.
"""

from __future__ import annotations

from typing import Any, Iterable

from deploy.cloud.common.manifest import production_limits

MIN_IDLE_SECONDS: int = 60
MAX_IDLE_SECONDS: int = 600
DEFAULT_IDLE_SECONDS: int = 120

# One job may run this long on the GPU; the gateway advertises the same deadline.
JOB_TIMEOUT_SECONDS: int = production_limits().worker_timeout_seconds
# A cold GPU container pulls a multi-gigabyte image and loads 5.5 GB of weights.
WORKER_STARTUP_TIMEOUT_SECONDS: int = 1200
# Seeding downloads the pinned snapshot once on a CPU container.
SEED_TIMEOUT_SECONDS: int = 3600

# Where Modal's request proxy for the gateway sits; the app's requests and the
# results it downloads go through it (https://modal.com/docs/guide/region-selection).
# The first is Modal's own default and what every installation had before the choice.
ROUTING_REGIONS = ("us-east", "us-west", "eu-west", "ap-south")
DEFAULT_ROUTING_REGION: str = ROUTING_REGIONS[0]

ENV_INSTALLATION_ID: str = "MC_INSTALLATION_ID"
DEFAULT_INSTALLATION_ID: str = "mc-local"


def parse_idle_seconds(value: Any) -> int:
    """Idle seconds before a warm GPU container scales to zero, within the allowed range."""
    if isinstance(value, bool):
        raise ValueError("idle_seconds must be an integer")
    try:
        seconds = int(str(value).strip()) if isinstance(value, str) else int(value)
    except (TypeError, ValueError) as exc:
        raise ValueError("idle_seconds must be an integer") from exc
    if isinstance(value, float) and value != seconds:
        raise ValueError("idle_seconds must be an integer")
    if not MIN_IDLE_SECONDS <= seconds <= MAX_IDLE_SECONDS:
        raise ValueError(f"idle_seconds must be between {MIN_IDLE_SECONDS} and {MAX_IDLE_SECONDS}")
    return seconds


def parse_routing_region(value: Any) -> str:
    """One of ROUTING_REGIONS, or ValueError."""
    if not isinstance(value, str) or value not in ROUTING_REGIONS:
        raise ValueError(f"routing_region must be one of {', '.join(ROUTING_REGIONS)}")
    return value


def parse_gpu(value: Any, allowlist: Iterable[str]) -> str:
    """The GPU name exactly as the provider spells it, or ValueError when not allowed."""
    allowed = tuple(allowlist)
    if not isinstance(value, str):
        raise ValueError(f"gpu must be one of: {', '.join(allowed)}")
    wanted = value.strip().upper()
    for name in allowed:
        if name.upper() == wanted:
            return name
    raise ValueError(f"gpu must be one of: {', '.join(allowed)}")
