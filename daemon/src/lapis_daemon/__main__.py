"""Module entrypoint for `python -m lapis_daemon`."""

from lapis_daemon.main import main

if __name__ == "__main__":
    raise SystemExit(main())
