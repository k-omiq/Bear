"""ASGI routing shared by the Beam gateway and its offline tests."""


def restore_gateway_path(gateway_app):
    async def app(scope, receive, send):
        if scope["type"] == "http":
            path = scope["path"]
            root_path = scope.get("root_path", "").rstrip("/")
            if root_path and not (path == root_path or path.startswith(root_path + "/")):
                scope = {**scope, "path": root_path + path}
        await gateway_app(scope, receive, send)

    return app


def mount_gateway_app(gateway_app):
    from starlette.applications import Starlette
    from starlette.routing import Mount

    routed_app = restore_gateway_path(gateway_app)
    return Starlette(routes=[
        Mount("/mc/v1", app=routed_app),
        Mount("/mc/analysis/v1", app=routed_app),
    ])
