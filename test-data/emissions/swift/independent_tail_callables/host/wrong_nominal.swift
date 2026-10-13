import CallbackA
import CallbackB

func wrongNominal<A: CallbackAHost, B: CallbackBHost>(
    _ value: KioNewtype_api__Secret<A, String>
) -> KioNewtype_peer__Secret<B, String> {
    value
}
