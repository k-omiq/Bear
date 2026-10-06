"""The fakes in fake_sdks.py against the real SDKs (skipped when the SDKs are not installed).

The driver tests only prove something if the fakes are called the way the real SDKs
are. So every fake callable the drivers use may take no more positional parameters
than the real one and only keywords the real one takes, every fake message may only
name real fields, and the exception classes the drivers catch must sit in the same
places of the hierarchy. Nothing here connects anywhere.

Run it where modal, beam and beta9 are installed (the helper build venv).
"""

import builtins
import dataclasses
import importlib.util
import inspect
import unittest
from typing import Any, List

from provisioner import fake_sdks

HAS_MODAL = importlib.util.find_spec("modal") is not None
HAS_BETA9 = importlib.util.find_spec("beta9") is not None and importlib.util.find_spec("beam") is not None


def _positional(parameters: List[inspect.Parameter]) -> List[str]:
    kinds = (inspect.Parameter.POSITIONAL_ONLY, inspect.Parameter.POSITIONAL_OR_KEYWORD)
    return [p.name for p in parameters if p.kind in kinds and p.name not in ("self", "cls")]


class SignatureChecks(unittest.TestCase):
    def assert_same_call_shape(self, fake: Any, real: Any, label: str) -> None:
        fake_params = list(inspect.signature(fake).parameters.values())
        real_params = list(inspect.signature(real).parameters.values())
        real_by_name = {p.name: p for p in real_params}
        real_takes_any_keyword = any(p.kind is p.VAR_KEYWORD for p in real_params)
        real_takes_any_positional = any(p.kind is p.VAR_POSITIONAL for p in real_params)
        fake_positional = _positional(fake_params)
        if not real_takes_any_positional:
            self.assertLessEqual(len(fake_positional), len(_positional(real_params)), f"{label}: too many positional parameters")
        # The drivers pass positional parameters by position; keyword-only ones must exist by name.
        for param in fake_params:
            if param.kind is not param.KEYWORD_ONLY:
                continue
            real_param = real_by_name.get(param.name)
            if real_param is None:
                self.assertTrue(real_takes_any_keyword, f"{label}: the real call has no parameter {param.name!r}")
            else:
                self.assertNotEqual(real_param.kind, real_param.POSITIONAL_ONLY, f"{label}: {param.name!r}")


@unittest.skipUnless(HAS_MODAL, "modal is not installed")
class ModalFidelity(SignatureChecks):
    def setUp(self) -> None:
        import modal
        import modal.experimental
        import modal.workspace

        self.modal = modal
        self.cloud = fake_sdks.FakeModalCloud()
        self.fake = self.cloud.sdk()

    def test_calls_the_driver_makes(self) -> None:
        modal, fake = self.modal, self.fake
        client = fake.Client.from_credentials("ak-good-token-id", "as-good-token-secret")
        workspace = fake.Workspace.from_context(client=client)
        store = fake.Dict.from_name("mc-ab12cd-jobs", client=client)
        function = fake.Function.from_name("mc-ab12cd", "gateway", client=client)
        pairs = {
            "Client.from_credentials": (fake.Client.from_credentials, modal.Client.from_credentials),
            "Client.hello": (client.hello, modal.Client.hello),
            "Workspace.from_context": (fake.Workspace.from_context, modal.Workspace.from_context),
            "Workspace.hydrate": (workspace.hydrate, modal.Workspace.hydrate),
            "Environment.from_context": (fake.Environment.from_context, modal.Environment.from_context),
            "proxy_tokens.create": (workspace.proxy_tokens.create, modal.workspace.WorkspaceProxyTokenManager.create),
            "proxy_tokens.allow": (workspace.proxy_tokens.allow, modal.workspace.WorkspaceProxyTokenManager.allow),
            "proxy_tokens.delete": (workspace.proxy_tokens.delete, modal.workspace.WorkspaceProxyTokenManager.delete),
            "Volume.objects.create": (fake.Volume.objects.create, modal.Volume.objects.create),
            "Volume.objects.delete": (fake.Volume.objects.delete, modal.Volume.objects.delete),
            "Dict.objects.create": (fake.Dict.objects.create, modal.Dict.objects.create),
            "Dict.objects.delete": (fake.Dict.objects.delete, modal.Dict.objects.delete),
            "Dict.from_name": (fake.Dict.from_name, modal.Dict.from_name),
            "Dict.get": (store.get, modal.Dict.get),
            "Dict.pop": (store.pop, modal.Dict.pop),
            "Function.from_name": (fake.Function.from_name, modal.Function.from_name),
            "Function.hydrate": (function.hydrate, modal.Function.hydrate),
            "Function.get_web_url": (function.get_web_url, modal.Function.get_web_url),
            "Function.spawn": (function.spawn, modal.Function.spawn),
            "Cls.from_name": (fake.Cls.from_name, modal.Cls.from_name),
            "FunctionCall.from_id": (fake.FunctionCall.from_id, modal.FunctionCall.from_id),
            "FunctionCall.get": (fake.FunctionCall.from_id("fc-1", client=client).get, modal.FunctionCall.get),
            "experimental.stop_app": (fake.experimental.stop_app, modal.experimental.stop_app),
            "experimental.list_deployed_apps": (fake.experimental.list_deployed_apps, modal.experimental.list_deployed_apps),
            "experimental.get_app_lifecycle": (fake.experimental.get_app_lifecycle, modal.experimental.get_app_lifecycle),
            "Volume.objects.list": (fake.Volume.objects.list, modal.Volume.objects.list),
            "Volume.from_name": (fake.Volume.from_name, modal.Volume.from_name),
            "Volume.listdir": (fake.Volume.from_name("mc-ab12cd-weights", client=client).listdir, modal.Volume.listdir),
            "Volume.info": (fake.Volume.from_name("mc-ab12cd-weights", client=client).info, modal.Volume.info),
            "Dict.put": (store.put, modal.Dict.put),
            "App.deploy": (fake_sdks.FakeModalApp.deploy, modal.App.deploy),
        }
        for label, (fake_call, real_call) in pairs.items():
            with self.subTest(label):
                self.assert_same_call_shape(fake_call, real_call, label)

    def test_token_data_fields(self) -> None:
        import modal.workspace

        real = {field.name for field in dataclasses.fields(modal.workspace.TokenData)}
        client = self.fake.Client.from_credentials("ak-good-token-id", "as-good-token-secret")
        issued = self.fake.Workspace.from_context(client=client).proxy_tokens.create()
        self.assertEqual(real, {"token_id", "token_secret"})
        self.assertTrue(all(hasattr(issued, name) for name in real))

    def test_listing_data_fields(self) -> None:
        import modal.experimental
        import modal.types

        # Discovery reads these attributes of what the real listings return.
        self.assertTrue({"app_id", "name"} <= {f.name for f in dataclasses.fields(modal.experimental.AppInfo)})
        self.assertIn("deployed_at", {f.name for f in dataclasses.fields(modal.experimental.AppLifecycle)})
        self.assertIn("created_at", {f.name for f in dataclasses.fields(modal.types.VolumeInfo)})
        self.assertIn("path", {f.name for f in dataclasses.fields(modal.types.FileEntry)})
        self.assertTrue(isinstance(getattr(modal.Volume, "name", None), property))

    def test_exception_hierarchy(self) -> None:
        import modal._functions
        import modal.exception as real

        fake = self.fake.exception
        for name in ("Error", "AuthError", "PermissionDeniedError", "NotFoundError", "InvalidError",
                     "AlreadyExistsError", "ConnectionError", "TimeoutError", "OutputExpiredError"):
            with self.subTest(name):
                self.assertTrue(issubclass(getattr(real, name), real.Error))
                self.assertTrue(issubclass(getattr(fake, name), fake.Error))
        for sub, base in (("OutputExpiredError", "TimeoutError"), ("InvalidError", "Error")):
            self.assertEqual(issubclass(getattr(real, sub), getattr(real, base)), issubclass(getattr(fake, sub), getattr(fake, base)))
        # Neither Modal TimeoutError is the builtin one, which a pending FunctionCall.get raises.
        self.assertFalse(issubclass(real.TimeoutError, builtins.TimeoutError))
        self.assertFalse(issubclass(fake.TimeoutError, builtins.TimeoutError))
        self.assertIs(vars(modal._functions).get("TimeoutError", builtins.TimeoutError), builtins.TimeoutError)
        self.assertIn("raise TimeoutError()", inspect.getsource(modal._functions._Invocation.poll_function))


@unittest.skipUnless(HAS_BETA9, "beam and beta9 are not installed")
class Beta9Fidelity(SignatureChecks):
    def setUp(self) -> None:
        from provisioner.beam_driver import load_beta9

        self.real = load_beta9()
        self.fake = fake_sdks.FakeBeamCloud().sdk()

    def test_messages_name_only_real_fields(self) -> None:
        groups = (
            (self.real.gateway, fake_sdks.GATEWAY_MESSAGES),
            (self.real.secret, fake_sdks.SECRET_MESSAGES),
            (self.real.volume, fake_sdks.VOLUME_MESSAGES),
            (self.real.map, fake_sdks.MAP_MESSAGES),
        )
        for module, messages in groups:
            for name, fields in messages.items():
                with self.subTest(name):
                    real_fields = {field.name for field in dataclasses.fields(getattr(module, name))}
                    self.assertLessEqual(set(fields), real_fields)

    def test_stubs_have_the_methods_the_driver_calls(self) -> None:
        stubs = (
            ("gateway", "GatewayServiceStub"),
            ("secret", "SecretServiceStub"),
            ("volume", "VolumeServiceStub"),
            ("map", "MapServiceStub"),
        )
        for module, stub in stubs:
            fake_stub = getattr(getattr(self.fake, module), stub)
            real_stub = getattr(getattr(self.real, module), stub)
            for name, method in vars(fake_stub).items():
                if name.startswith("_") or not callable(method):
                    continue
                with self.subTest(f"{stub}.{name}"):
                    self.assertTrue(hasattr(real_stub, name))
                    self.assert_same_call_shape(method, getattr(real_stub, name), f"{stub}.{name}")

    def test_channel_config_and_helpers(self) -> None:
        real, fake = self.real, self.fake
        pairs = {
            "Channel": (fake.Channel.__init__, real.Channel.__init__),
            "ConfigContext": (fake.ConfigContext.__init__, real.ConfigContext.__init__),
            "rpc_timeout": (fake.rpc_timeout, real.rpc_timeout),
            "set_channel": (fake.base.set_channel, real.base.set_channel),
            "unset_channel": (fake.base.unset_channel, real.base.unset_channel),
            "redirect_terminal_to_buffer": (fake.terminal.redirect_terminal_to_buffer, real.terminal.redirect_terminal_to_buffer),
        }
        for label, (fake_call, real_call) in pairs.items():
            with self.subTest(label):
                self.assert_same_call_shape(fake_call, real_call, label)

    def test_handler_deploy(self) -> None:
        from beta9.abstractions.mixins import DeployableMixin

        self.assert_same_call_shape(fake_sdks.FakeBeamHandler.deploy, DeployableMixin.deploy, "deploy")


if __name__ == "__main__":
    unittest.main()
