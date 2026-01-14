#!/usr/bin/env bash

while true; do echo -e "HTTP/1.1 200 OK\\r\\n\\r\\nHello from Netcat" | nc -l 1234; done
