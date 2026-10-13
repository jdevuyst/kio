import CallbackA

func forgePrivate<H: CallbackAHost>(_: H.Type) -> KioNewtype_api__Secret<H, String> {
    KioNewtype_api__Secret<H, String>("forged")
}
