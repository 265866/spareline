"""A stand-in adb server for the demo: three invented phones with invented SIMs, numbers and
texts, answered in the formats real phones print. It listens on loopback only and talks to no
phone and no real adb server, so spareline behind it ingests and shows nothing real.

    python3 -I adb.py PORT

Every time it prints is an offset from when it started, so each run shows the same picture
shifted to now. The texts reach the phones' providers one at a time, oldest first, every
REVEAL_EVERY seconds after REVEAL_AFTER, so spareline ingests them in the order they were sent,
as it would have live. Commands it doesn't know get exit 0 and no output, and are listed on stderr.
"""

import socketserver
import sys
import threading
import time

START_MS = int(time.time() * 1000)
# The phone manager reads each phone first; ingest compares `dumpsys isms` every 3 s.
REVEAL_AFTER = 6.0
REVEAL_EVERY = 3.5

# The make, model and build are invented too. Numbers come from 555-0100 to 555-0199, which the
# North American Numbering Plan keeps for fiction. ICCIDs, IMEIs and serials are made up, and the network code 001-01 is the test network.
# An IMEI is listed without its check digit, which `with_check_digit` adds.
PHONES = [
    {
        "serial": "DEMO0A11CE01",
        "state": "device",
        "model": "RW-5",
        "battery": 86,
        "imeis": ["35000000000010", "35000000000011"],
        "sims": [
            {"slot": 0, "sub": 1, "iccid": "8900100000000000011", "number": "+15125550142",
             "carrier": "Larkspur Mobile", "short": "Larkspur", "rsrp": -94, "level": 3},
            {"slot": 1, "sub": 2, "iccid": "8900100000000000029", "number": "+15125550177",
             "carrier": "Brightwave", "short": "Brightwave", "rsrp": -103, "level": 2},
        ],
    },
    {
        "serial": "DEMO0B22CE02",
        "state": "device",
        "model": "RW-5",
        "battery": 61,
        "imeis": ["35000000000020", "35000000000021"],
        "sims": [
            {"slot": 0, "sub": 3, "iccid": "8900100000000000037", "number": "+17375550118",
             "carrier": "Larkspur Mobile", "short": "Larkspur", "rsrp": -88, "level": 4},
        ],
    },
    {
        # Plugged in, but its owner hasn't tapped "Allow" on the phone yet.
        "serial": "DEMO0C33CE03",
        "state": "unauthorized",
    },
]

MINUTE = 60_000

# The texts each SIM has received: minutes before the start, the sender, and the body.
TEXTS = {
    1: [
        (2950, "+15125550163", "Hi! Is the road bike still for sale? I could pick it up Saturday morning."),
        (2940, "+15125550163", "Also, does it come with the lights?"),
        (1460, "+15125550109", "Saw your listing. Would you take $180 for the bike?"),
        (95, "+15125550163", "Great, see you at 10. I'll bring cash."),
        (12, "Fernbank", "Fernbank: your verification code is 418207. It expires in 10 minutes."),
    ],
    2: [
        (4310, "Juniper", "Juniper & Co.: your order #20417 has shipped."),
        (1720, "Juniper", "Juniper & Co.: your package is out for delivery today."),
        (300, "+15125550188", "Reminder: dentist appointment Thursday at 3:40 PM. Reply C to confirm."),
        (41, "Fernbank", "Fernbank: a new device signed in to your account. Wasn't you? Reply STOP."),
    ],
    3: [
        (5800, "Marigold", "Welcome to the Marigold Bakery newsletter! Reply STOP to unsubscribe."),
        (2890, "Marigold", "Marigold Bakery: sourdough is back this weekend. Pre-order by Friday."),
        (130, "+17375550131", "Hey, this is Sam from the bakery list. Are you still hiring for weekends?"),
        (7, "Marigold", "Marigold Bakery: your code is 9046."),
    ],
}


def with_check_digit(body):
    """`body` and its Luhn check digit, which IMEIs end with."""
    total = 0
    for index, digit in enumerate(int(c) for c in reversed(body)):
        doubled = digit * 2 if index % 2 == 0 else digit
        total += doubled - 9 if doubled > 9 else doubled
    return body + str((10 - total % 10) % 10)


def message_rows(sub):
    """The SMS provider rows of one SIM, oldest first, with ids from 1."""
    rows = []
    for index, (minutes, sender, body) in enumerate(sorted(TEXTS[sub], key=lambda t: -t[0])):
        at = START_MS - minutes * MINUTE
        rows.append({"id": index + 1, "address": sender, "date": at, "sent": at - 4000, "body": body})
    return rows


def revealed_count():
    """How many texts, of all phones, oldest first, the providers hold by now."""
    return max(0, int((time.time() * 1000 - START_MS) / 1000 / REVEAL_EVERY - REVEAL_AFTER / REVEAL_EVERY) + 1)


def all_rows():
    """Every SMS row of every phone, oldest first, with the phone's serial."""
    rows = []
    for phone in PHONES:
        for sim in phone.get("sims", []):
            rows += [{**row, "sub": sim["sub"], "serial": phone["serial"]} for row in message_rows(sim["sub"])]
    return sorted(rows, key=lambda row: row["date"])


def phone_rows(phone):
    """The SMS rows a phone's provider holds by now, with ids in the order they arrived."""
    visible = all_rows()[: revealed_count()]
    rows = [row for row in visible if row["serial"] == phone["serial"]]
    return [{**row, "id": index + 1} for index, row in enumerate(rows)]


def identity(phone):
    sims = {sim["slot"]: sim for sim in phone["sims"]}
    states = ",".join("LOADED" if slot in sims else "ABSENT" for slot in (0, 1))
    props = [
        "ro.product.manufacturer=Ridgeway",
        f"ro.product.model={phone['model']}",
        "ro.build.version.release=14",
        "ro.build.version.sdk=34",
        "ro.build.fingerprint=Ridgeway/rw5/rw5:14/DEMO.000101.001/1000001:user/release-keys",
        "ro.build.version.security_patch=2026-01-05",
        "persist.radio.multisim.config=dsds",
        f"gsm.sim.state={states}",
    ]
    first = phone["sims"][0]["sub"]
    isub = ["SubscriptionManagerService:", "Active modem count=2", "Logical SIM slot sub id mapping:"]
    isub += [f"  Logical SIM slot {sim['slot']}: subId={sim['sub']}" for sim in phone["sims"]]
    isub += [
        "",
        f"defaultSubId={first}",
        f"defaultVoiceSubId={first}",
        f"defaultDataSubId={first}",
        f"activeDataSubId={first}",
        f"defaultSmsSubId={first}",
        "areAllSubscriptionsLoaded=true",
        "",
    ]
    isub += [f"mSimState[{slot}]={'LOADED' if slot in sims else 'ABSENT'}" for slot in (0, 1)]
    isub += ["", "Active subscriptions:"]
    for sim in phone["sims"]:
        masked = sim["iccid"][:9] + "[****]"
        isub.append(
            f"  [SubscriptionInfoInternal: id={sim['sub']} iccId={masked} simSlotIndex={sim['slot']} "
            f"portIndex=0 isEmbedded=0 carrierId=1 displayName={sim['carrier']} "
            f"carrierName={sim['carrier']} number=[****] dataRoaming=0 mcc=001 mnc=01 "
            f"cardString={masked} cardId={sim['slot']} countryIso=us type=LOCAL_SIM]"
        )
    return "\n@@sl:props\n" + "\n".join(props) + "\n\n@@sl:isub\n" + "\n".join(isub) + "\n"


def parcel(text):
    """`service call` output for a successful String16 reply, as Android prints it."""
    data = [0, len(text)] if text is not None else [0, -1]
    units = [ord(c) for c in text] + [0] if text is not None else []
    if len(units) % 2:
        units.append(0)
    words = [value & 0xFFFFFFFF for value in data]
    words += [units[i] | (units[i + 1] << 16) for i in range(0, len(units), 2)]
    lines = []
    for offset in range(0, len(words), 4):
        chunk = words[offset:offset + 4]
        raw = b"".join(word.to_bytes(4, "little") for word in chunk)
        ascii_column = "".join(chr(b) if 32 <= b < 127 else "." for b in raw)
        hex_column = " ".join(f"{word:08x}" for word in chunk)
        end = "')" if offset + 4 >= len(words) else "'"
        lines.append(f"0x{offset * 4:08x}: {hex_column:<35} '{ascii_column:<16}{end}")
    return "Result: Parcel(\t\n" + "\n".join(lines) + "\n"


def subscriber(phone):
    parts = []
    for slot, imei in enumerate(phone["imeis"]):
        parts.append(f"\n@@sl:imei:{slot}\n" + parcel(with_check_digit(imei)))
    for sim in phone["sims"]:
        parts.append(f"\n@@sl:iccid:{sim['sub']}\n" + parcel(sim["iccid"]))
        parts.append(f"\n@@sl:number:{sim['sub']}\n" + parcel(sim["number"]))
    return "".join(parts)


def health(phone):
    sims = {sim["slot"]: sim for sim in phone["sims"]}
    battery = [
        "Current Battery Service state:",
        "  AC powered: false",
        "  USB powered: true",
        "  Wireless powered: false",
        "  status: 2",
        "  health: 2",
        "  present: true",
        f"  level: {phone['battery']}",
        "  scale: 100",
        "  voltage: 4103",
        "  temperature: 252",
        "  technology: Li-poly",
    ]
    registry = []
    for slot in (0, 1):
        registry.append(f"  Phone Id={slot}")
        sim = sims.get(slot)
        if sim:
            registry += [
                f"    mServiceState={{mVoiceRegState=0(IN_SERVICE), mDataRegState=0(IN_SERVICE), "
                f"mOperatorAlphaLong={sim['carrier']}, mOperatorAlphaShort={sim['short']}, "
                f"getRilVoiceRadioTechnology=14(LTE)}}",
                f"    mSignalStrength=SignalStrength:{{mLte=CellSignalStrengthLte: rsrp={sim['rsrp']} "
                f"rsrq=-11 level={sim['level']},primary=CellSignalStrengthLte}}",
                "    mDataConnectionState=0",
                "    mIsDataEnabled=false",
            ]
        else:
            registry += [
                "    mServiceState={mVoiceRegState=3(POWER_OFF), mDataRegState=3(POWER_OFF), "
                "mOperatorAlphaLong=null, mOperatorAlphaShort=null}",
                "    mDataConnectionState=-1",
                "    mIsDataEnabled=false",
            ]
    states = ",".join("LOADED" if slot in sims else "ABSENT" for slot in (0, 1))
    return (
        "\n@@sl:battery\n" + "\n".join(battery)
        + f"\n\n@@sl:simstate\n{states}\n\n@@sl:registry\n" + "\n".join(registry) + "\n"
    )


def sms_query(phone, command):
    rows = phone_rows(phone)
    if "--projection _id:date " in command:
        lines = [f"Row: {i} _id={row['id']}, date={row['date']}" for i, row in enumerate(rows)]
        return "\n".join(lines) + "\n" if lines else "No result found.\n"
    if "--where '_id>" in command:
        after = int(command.split("--where '_id>")[1].split("'")[0])
        rows = [row for row in rows if row["id"] > after]
    elif "--where '_id IN (" in command:
        wanted = command.split("--where '_id IN (")[1].split(")")[0]
        ids = {int(part) for part in wanted.split(",") if part}
        rows = [row for row in rows if row["id"] in ids]
    lines = [
        f"Row: {i} _id={row['id']}, thread_id={row['sub']}, address={row['address']}, "
        f"date={row['date']}, date_sent={row['sent']}, sub_id={row['sub']}, read=1, type=1, "
        f"hex(body)={row['body'].encode().hex().upper()}"
        for i, row in enumerate(rows)
    ]
    return "\n".join(lines) + "\n" if lines else "No result found.\n"


def answer(phone, command):
    """What `command` prints on `phone`, or None to keep the stream open without output."""
    if command.startswith("printf '\\n@@sl:props\\n'"):
        return identity(phone)
    if command.startswith("printf '\\n@@sl:imei:0\\n'"):
        return subscriber(phone)
    if command.startswith("printf '\\n@@sl:battery\\n'"):
        return health(phone)
    if command.startswith("content query --uri content://sms/inbox"):
        return sms_query(phone, command)
    if command.startswith("content query --uri content://mms"):
        return "No result found.\n"
    if command == "getprop gsm.sim.state":
        sims = {sim["slot"] for sim in phone["sims"]}
        return ",".join("LOADED" if slot in sims else "ABSENT" for slot in (0, 1)) + "\n"
    if command.startswith("dumpsys isms"):
        return f"      GsmInboundSmsHandler:\n       total records={len(phone_rows(phone))}\n"
    if command.startswith("dumpsys power"):
        return "  mWakefulness=Awake\n"
    if command.startswith("logcat") and " -G " not in command:
        return None
    if command not in ("true",):
        print(f"unanswered: {command[:160]}", file=sys.stderr, flush=True)
    return ""


def packet(kind, data):
    return bytes([kind]) + len(data).to_bytes(4, "little") + data


class Handler(socketserver.BaseRequestHandler):
    def read_request(self):
        header = self.recv_exact(4)
        if header is None:
            return None
        return self.recv_exact(int(header.decode(), 16)).decode(errors="replace")

    def recv_exact(self, length):
        data = b""
        while len(data) < length:
            chunk = self.request.recv(length - len(data))
            if not chunk:
                return None
            data += chunk
        return data

    def okay(self, text=None):
        reply = b"OKAY"
        if text is not None:
            data = text.encode()
            reply += f"{len(data):04x}".encode() + data
        self.request.sendall(reply)

    def fail(self, message):
        data = message.encode()
        self.request.sendall(b"FAIL" + f"{len(data):04x}".encode() + data)

    def handle(self):
        try:
            self.serve()
        except (OSError, ValueError):
            pass

    def serve(self):
        request = self.read_request()
        if request is None:
            return
        listing = "".join(f"{phone['serial']}\t{phone['state']}\n" for phone in PHONES)
        if request == "host:version":
            self.okay("0029")
        elif request in ("host:devices", "host:devices-l"):
            self.okay(listing)
        elif request in ("host:track-devices", "host:track-devices-l"):
            self.okay(listing)
            while self.request.recv(1024):
                pass
        elif request.startswith("host:transport:"):
            serial = request.removeprefix("host:transport:")
            phone = next((p for p in PHONES if p["serial"] == serial), None)
            if phone is None:
                self.fail(f"device '{serial}' not found")
                return
            if phone["state"] != "device":
                self.fail(f"device {phone['state']}")
                return
            self.okay()
            self.device(phone, self.read_request() or "")
        else:
            self.fail(f"{request}: not served by the demo")

    def device(self, phone, service):
        if service.startswith("shell,v2,raw:"):
            output = answer(phone, service.removeprefix("shell,v2,raw:"))
            self.okay()
            if output is None:
                while self.request.recv(1024):
                    pass
                return
            self.request.sendall(packet(1, output.encode()) + packet(3, b"\x00"))
        elif service.startswith("exec:"):
            self.okay()
        else:
            self.fail(f"{service.split(':')[0]}: not served by the demo")


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    server = Server(("127.0.0.1", int(sys.argv[1])), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    print(f"demo adb on 127.0.0.1:{sys.argv[1]}", file=sys.stderr, flush=True)
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        pass
