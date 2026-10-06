"""Entrypoint for `python -m provisioner`.

Executes the strict helper protocol runner.
"""

import sys
from provisioner.cli import main

if __name__ == "__main__":
    sys.exit(main())
