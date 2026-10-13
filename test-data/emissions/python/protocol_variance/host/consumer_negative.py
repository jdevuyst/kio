from protocol_variance import ProtocolVarianceHost


class Token:
    pass


def widen(host: ProtocolVarianceHost[Token]) -> ProtocolVarianceHost[object]:
    return host


def narrow(host: ProtocolVarianceHost[object]) -> ProtocolVarianceHost[Token]:
    return host
