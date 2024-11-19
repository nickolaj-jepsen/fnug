import click
from click_default_group import DefaultGroup

from fnug.core import FnugCore
from fnug.logging import LogLevel, get_logger, log_level_callback, setup_logging
from fnug.ui.app import FnugApp

DEFAULT_FILE_NAMES = [".fnug.json", ".fnug.yaml", ".fnug.yml"]

logger = get_logger()


@click.group(cls=DefaultGroup, default="run", default_if_no_args=True, invoke_without_command=True)
@click.option(
    "--verbose",
    expose_value=False,
    flag_value=LogLevel.INFO,
    is_flag=True,
    help="Verbose output",
    callback=log_level_callback,
)
@click.option(
    "--quiet",
    expose_value=False,
    flag_value=LogLevel.ERROR,
    is_flag=True,
    help="Quiet output",
    callback=log_level_callback,
)
@click.version_option()
def cli() -> None:
    """Entrypoint for the fnug CLI."""
    setup_logging()


@cli.command()  # pyright: ignore reportUnknownMemberType
@click.option("--config", "-c", type=click.Path(), help="Config file")
def run(config: str | None = None) -> None:
    """Run the fnug application."""
    FnugApp.from_config_file(config).run()


@cli.command()  # pyright: ignore reportUnknownMemberType
@click.option("--config", "-c", type=click.Path(), help="Config file")
def config(config: str | None = None) -> None:
    """Print the current configuration."""
    click.echo(FnugCore.from_config_file(config).config.as_yaml())
