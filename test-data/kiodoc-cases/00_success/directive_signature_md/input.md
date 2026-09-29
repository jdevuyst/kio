# Directive test in .md prose

The [`@signature print`] directive embeds the declaration header of `print` inline.

The [`@source print`] directive block-promotes the full source of `print`.

The [`@type print`] directive embeds the bound-value type of `print`.

A fn is also a valid directive target: [`@signature compute`].

Fully-qualified paths also resolve: [`@signature mdpkg.util.something`].

A directive term with a Markdown override is accepted too.
See [`@signature overridden`] for details.

[overridden]: https://example.com
