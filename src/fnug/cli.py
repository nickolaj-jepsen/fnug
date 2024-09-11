from pathlib import Path

import click

from fnug import FnugApp

DEFAULT_FILE_NAMES = [".fnug.json", ".fnug.yaml", ".fnug.yml"]


@click.command()
@click.option("--config", "-c", type=click.Path(), help="Config file")
@click.option("--verbose", "-v", is_flag=True, help="Verbose output")
@click.version_option()
def cli(config: str | None = None, verbose: bool = False) -> None:
    """Entrypoint for the fnug CLI."""
    from fnug.core import FnugCore

    core = FnugCore(config)
    FnugApp(core.config, cwd=Path(core.cwd).resolve()).run()
