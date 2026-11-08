"""Test-only RFC 4256 peer using Paramiko's real SSH transport and signatures."""

import socket
import sys
import threading

import paramiko
from paramiko.auth_handler import AuthHandler


class FixtureAuthHandler(AuthHandler):
    def _send_auth_result(self, username, method, result):
        # The publickey callback also sees unsigned probes. This result hook
        # runs after Paramiko has verified the signature on the real request.
        if method == "publickey" and result == paramiko.AUTH_PARTIALLY_SUCCESSFUL:
            self.transport.server_object.public_key_verified = True
        return super()._send_auth_result(username, method, result)


class FixtureTransport(paramiko.Transport):
    def _parse_newkeys(self, message):
        if self.server_mode and self.auth_handler is None:
            self.auth_handler = FixtureAuthHandler(self)
        return super()._parse_newkeys(message)


class Peer(paramiko.ServerInterface):
    def __init__(self, mode, allowed_key):
        self.mode = mode
        self.allowed_key = allowed_key
        self.public_key_verified = False
        self.saw_none = False
        self.saw_password = False
        self.round = 0
        self.command = threading.Event()

    def get_allowed_auths(self, username):
        if self.mode == "combined" and not self.public_key_verified:
            return "publickey"
        if self.mode == "password-first" and not self.saw_password:
            return "password"
        return "keyboard-interactive"

    def check_auth_none(self, username):
        self.saw_none = True
        return paramiko.AUTH_FAILED

    def check_auth_password(self, username, password):
        self.saw_password = username == "test-user" and password == "password-method-test-value"
        return paramiko.AUTH_FAILED

    def check_auth_publickey(self, username, key):
        if self.mode == "combined" and key.get_base64() == self.allowed_key:
            return paramiko.AUTH_PARTIALLY_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_auth_interactive(self, username, submethods):
        if (
            username != "test-user"
            or submethods
            or self.get_allowed_auths(username) != "keyboard-interactive"
        ):
            return paramiko.AUTH_FAILED
        if self.mode == "no-challenge":
            return paramiko.AUTH_SUCCESSFUL
        if self.mode == "too-many-prompts":
            return paramiko.InteractiveQuery("Too many", "", *["test prompt"] * 17)
        if self.mode == "too-long-answer":
            return paramiko.InteractiveQuery("Bounded input", "", ("Answer: ", False))
        return paramiko.InteractiveQuery("Server notice", "Complete the sign-in.")

    def check_auth_interactive_response(self, responses):
        self.round += 1
        print(f"round-response {self.round}", flush=True)
        if self.mode == "too-many-rounds":
            return paramiko.InteractiveQuery("Extra round", "")
        if self.round == 1 and responses == []:
            return paramiko.InteractiveQuery(
                "Two prompts", "Both are needed.", ("Code\x1b\u202e: ", False), ("Reason: ", True)
            )
        if self.round == 2 and responses == ["ä123", ""]:
            return paramiko.InteractiveQuery("Final prompt", "", ("Confirm: ", True))
        if self.round == 3 and responses == ["yes"]:
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_channel_request(self, kind, chanid):
        if kind == "session":
            return paramiko.OPEN_SUCCEEDED
        return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_exec_request(self, channel, command):
        if command != b"fixture-command":
            return False
        self.command.set()
        return True

    def check_channel_pty_request(self, channel, term, width, height, pixel_width, pixel_height, modes):
        return True

    def check_channel_shell_request(self, channel):
        self.command.set()
        return True


def serve(mode, host_key, allowed_key):
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        listener.settimeout(12)
        print(listener.getsockname()[1], flush=True)
        connection, _ = listener.accept()
        with connection, FixtureTransport(connection, strict_kex=True) as transport:
            transport.add_server_key(paramiko.Ed25519Key.from_private_key_file(host_key))
            peer = Peer(mode, allowed_key)
            refused_key = mode in ("unknown", "changed")
            try:
                transport.start_server(server=peer)
            except EOFError:
                assert not refused_key or not peer.saw_none
                return
            except ConnectionResetError:
                if not refused_key:
                    raise
                assert not peer.saw_none
                return
            channel = transport.accept(6)
            if refused_key:
                assert not peer.saw_none
            if channel is None:
                return
            with channel:
                if not peer.command.wait(4):
                    raise TimeoutError("test exec did not arrive")
                channel.sendall(b"keyboard-interactive-ok\n")
                channel.send_exit_status(23)
                channel.shutdown_write()


if __name__ == "__main__":
    serve(*sys.argv[1:])
