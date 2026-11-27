#!/usr/bin/env python3
"""A stand-in for Tor used by tests/e2e/m9.sh.

It offers the two things Swarmgit uses from Tor:

* a SOCKS5 proxy (CONNECT only, no authentication) on --socks;
* a control port on --control that understands PROTOCOLINFO, AUTHENTICATE,
  ADD_ONION and DEL_ONION. A service added there is reachable through the
  SOCKS port as <id>.onion while its control connection stays open.

--map NAME=HOST gives the proxy a private name that only it can resolve, so a
test can show that names are looked up at the far end and never locally.
Every request is appended to --log.
"""

import argparse
import hashlib
import socket
import socketserver
import struct
import threading

onions = {}  # "<id>.onion" -> {virtual port: (host, port)}
names = {}
log_path = None
log_lock = threading.Lock()


def log(line):
    with log_lock:
        with open(log_path, "a", encoding="utf-8") as out:
            out.write(line + "\n")


def read_exact(sock, count):
    data = b""
    while len(data) < count:
        chunk = sock.recv(count - len(data))
        if not chunk:
            raise ConnectionError("closed")
        data += chunk
    return data


def pump(source, sink):
    try:
        while True:
            chunk = source.recv(65536)
            if not chunk:
                break
            sink.sendall(chunk)
    except OSError:
        pass
    finally:
        try:
            sink.shutdown(socket.SHUT_WR)
        except OSError:
            pass


class Socks(socketserver.BaseRequestHandler):
    def handle(self):
        client = self.request
        version, count = read_exact(client, 2)
        read_exact(client, count)
        if version != 5:
            return
        client.sendall(b"\x05\x00")
        _, command, _, kind = read_exact(client, 4)
        if kind == 1:
            host = socket.inet_ntop(socket.AF_INET, read_exact(client, 4))
        elif kind == 4:
            host = socket.inet_ntop(socket.AF_INET6, read_exact(client, 16))
        else:
            host = read_exact(client, read_exact(client, 1)[0]).decode()
        port = struct.unpack(">H", read_exact(client, 2))[0]
        if command != 1:
            client.sendall(b"\x05\x07\x00\x01" + bytes(6))
            return
        log(f"CONNECT {host}:{port}")
        if host.endswith(".onion"):
            target = onions.get(host, {}).get(port)
        else:
            target = (names.get(host, host), port)
        try:
            if target is None:
                raise OSError("unknown onion service")
            upstream = socket.create_connection(target, timeout=10)
        except OSError as err:
            log(f"FAILED {host}:{port} {err}")
            client.sendall(b"\x05\x04\x00\x01" + bytes(6))
            return
        upstream.settimeout(None)
        client.sendall(b"\x05\x00\x00\x01" + bytes(6))
        back = threading.Thread(target=pump, args=(upstream, client), daemon=True)
        back.start()
        pump(client, upstream)
        back.join()
        upstream.close()


class Control(socketserver.StreamRequestHandler):
    def reply(self, *lines):
        self.wfile.write("".join(f"{line}\r\n" for line in lines).encode())
        self.wfile.flush()

    def handle(self):
        mine = []
        authenticated = False
        try:
            for raw in self.rfile:
                line = raw.decode().strip()
                verb, _, rest = line.partition(" ")
                verb = verb.upper()
                if verb == "PROTOCOLINFO":
                    self.reply("250-PROTOCOLINFO 1", "250-AUTH METHODS=NULL",
                               '250-VERSION Tor="0.4.8.0"', "250 OK")
                elif verb == "AUTHENTICATE":
                    authenticated = True
                    self.reply("250 OK")
                elif not authenticated:
                    self.reply("514 Authentication required")
                    return
                elif verb == "ADD_ONION":
                    parts = rest.split()
                    key = parts[0]
                    new = key.startswith("NEW:")
                    if new:
                        key = "ED25519-V3:" + hashlib.sha256(
                            f"{id(self)}".encode()).hexdigest()
                    digest = hashlib.sha256(key.encode()).digest()
                    service = "".join(
                        "abcdefghijklmnopqrstuvwxyz234567"[byte % 32]
                        for byte in digest + digest[:24])
                    ports = {}
                    for part in parts[1:]:
                        if part.startswith("Port="):
                            virtual, _, target = part[5:].partition(",")
                            host, _, port = target.rpartition(":")
                            ports[int(virtual)] = (host, int(port))
                    onions[service + ".onion"] = ports
                    mine.append(service + ".onion")
                    log(f"ADD_ONION {service}.onion {'new' if new else 'saved'}")
                    lines = [f"250-ServiceID={service}"]
                    if new:
                        lines.append(f"250-PrivateKey={key}")
                    self.reply(*lines, "250 OK")
                elif verb == "DEL_ONION":
                    onions.pop(rest + ".onion", None)
                    self.reply("250 OK")
                else:
                    self.reply("510 Unrecognized command")
        finally:
            # Like Tor, a service without Detach ends with its connection.
            for service in mine:
                onions.pop(service, None)


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def address(text):
    host, _, port = text.rpartition(":")
    return host, int(port)


def main():
    global log_path
    parser = argparse.ArgumentParser()
    parser.add_argument("--socks", required=True, type=address)
    parser.add_argument("--control", type=address)
    parser.add_argument("--map", action="append", default=[])
    # A service set up in torrc: NAME.onion:PORT=HOST:PORT.
    parser.add_argument("--onion", action="append", default=[])
    parser.add_argument("--log", required=True)
    args = parser.parse_args()
    log_path = args.log
    for entry in args.map:
        name, _, host = entry.partition("=")
        names[name] = host
    for entry in args.onion:
        virtual, _, target = entry.partition("=")
        name, port = address(virtual)
        onions.setdefault(name, {})[port] = address(target)
    if args.control:
        control = Server(args.control, Control)
        threading.Thread(target=control.serve_forever, daemon=True).start()
    with Server(args.socks, Socks) as socks:
        log("READY")
        socks.serve_forever()


if __name__ == "__main__":
    main()
