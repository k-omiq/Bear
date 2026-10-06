"""Manga Cleaner cloud provisioner: the helper that sets up Modal or Beam.

The desktop runs it once per operation (`python -m provisioner` or the frozen
`manga-cleaner-provisioner` binary) with one JSON request on stdin; see cli.py for
the stream contract and controller.py for the operations.

This package init stays import-free: the CLI must isolate stdout and stderr and
harden the environment before any provider SDK or the deploy package is imported.
"""

from importlib import import_module
from typing import Any

__version__ = "0.3.0"

_EXPORTS = {
    "ProvisioningController": "provisioner.controller",
    "ModalDriver": "provisioner.modal_driver",
    "BeamDriver": "provisioner.beam_driver",
    "InstallationJournal": "provisioner.journal",
    "InstallationRecord": "provisioner.journal",
    "ResourceRecord": "provisioner.journal",
    "HELPER_PROTOCOL_VERSION": "provisioner.protocol",
    "ProtocolError": "provisioner.protocol",
    "parse_request": "provisioner.protocol",
    "make_success_response": "provisioner.protocol",
    "make_error_response": "provisioner.protocol",
    "redact_data": "provisioner.redaction",
    "redact_string": "provisioner.redaction",
    "main": "provisioner.cli",
    "run_helper": "provisioner.cli",
}

__all__ = sorted(_EXPORTS) + ["__version__"]


def __getattr__(name: str) -> Any:
    module = _EXPORTS.get(name)
    if module is None:
        raise AttributeError(f"module 'provisioner' has no attribute '{name}'")
    return getattr(import_module(module), name)
