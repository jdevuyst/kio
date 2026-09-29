import { createApp, type AppHost } from "../workdir/out/ts/app.js";

const newHost: AppHost = {
  api: {
    open: () => "live",
  },
};

const pkg = createApp(newHost);
const echoed: string = pkg.api.echo("new host supplies only live items");
void echoed;
