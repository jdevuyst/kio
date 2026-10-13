import CallbackA

func readPrivate<H: CallbackAHost>(_ value: KioNewtype_api__Secret<H, String>) {
    _ = value.__kioValue
}
