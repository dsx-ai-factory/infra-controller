#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# Echo data back in a way that impi expects for things like `sol activate`.

import socket
import threading

HOST = "127.0.0.1"
PORT = 9003

def handle_client(conn, addr):
    print(f"Connected by {addr}")
    with conn:
        buffer = b""
        while True:
            data = conn.recv(1024)
            if not data:
                print(f"Connection closed by {addr}")
                break
            print(f"Received from {addr}: {data!r}")

            conn.sendall(data)

            buffer += data
            if buffer.endswith(b"\n") or buffer.endswith(b"\r"):
                buffer = b""
                # Emulate an `ed` session (https://www.gnu.org/fun/jokes/ed-msg.html) :-D
                conn.sendall(b"\r\n?\r\n")

def main():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind((HOST, PORT))
        s.listen()
        print(f"Listening on port {PORT}...")
        while True:
            conn, addr = s.accept()
            thread = threading.Thread(target=handle_client, args=(conn, addr), daemon=True)
            thread.start()

if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        exit
