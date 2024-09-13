import click

from fnug import FnugApp
from fnug.core import FnugCore

DEFAULT_FILE_NAMES = [".fnug.json", ".fnug.yaml", ".fnug.yml"]


@click.command()
@click.option("--config", "-c", type=click.Path(), help="Config file")
@click.option("--verbose", "-v", is_flag=True, help="Verbose output")
@click.version_option()
def cli(config: str | None = None, verbose: bool = False) -> None:
    """Entrypoint for the fnug CLI."""
    core = FnugCore(config)
    FnugApp(core).run()
