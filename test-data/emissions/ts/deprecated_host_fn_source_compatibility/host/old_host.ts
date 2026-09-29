import {
  createApp,
  type AppHost,
  type AppHostTypes,
  type AppTypeLambda,
} from "../workdir/out/ts/app.js";

type NativeBox<A> = { readonly value: A };

interface NativeBoxK extends AppTypeLambda<readonly [unknown]> {
  readonly type: NativeBox<this["arguments"][0]>;
}

interface OldBindings extends AppHostTypes {
  readonly api: {
    readonly Box: NativeBoxK;
  };
  readonly legacy: {
    readonly Gone: string;
  };
}

const oldHost: AppHost<OldBindings> = {
  api: {
    open: () => "live",
    old: (head, value) => {
      head.toUpperCase();
      value._0.toUpperCase();
      const token: typeof value._1.value = { Token: null };
      return { value: token };
    },
  },
  legacy: {
    retired: value => value.toUpperCase(),
  },
};

const pkg = createApp<OldBindings>(oldHost);
const echoed: string = pkg.api.echo("old host remains valid");
void echoed;
