import types

import app


host = types.SimpleNamespace(
    KioHostIn_api_Str=lambda value: value,
    KioHostOut_api_Str=lambda value: value,
    api=types.SimpleNamespace(open=lambda: "live"),
)
pkg = app.create_app(host)
assert pkg.api.echo("live") == "live"
