from pathlib import Path

import click

from fnug import FnugApp
from fnug.config import load_config

DEFAULT_FILE_NAMES = [".fnug.json", ".fnug.yaml", ".fnug.yml"]


@click.command()
@click.option("--config", "-c", type=click.Path(), help="Config file")
@click.option("--verbose", "-v", is_flag=True, help="Verbose output")
@click.version_option()
def cli(config: str | None = None, verbose: bool = False) -> None:
    """Entrypoint for the fnug CLI."""
    try:
        if config is None:
            for file_name in DEFAULT_FILE_NAMES:
                if Path(file_name).exists():
                    config = file_name
                    break
            else:
                raise click.ClickException(f"Could not find a config file. Tried: {', '.join(DEFAULT_FILE_NAMES)}")

        file_path = Path(config)
        cwd = file_path.parent
        cfg = load_config(file_path)
        FnugApp(cfg, cwd=cwd).run()
    except Exception as e:
        if verbose:
            raise
        raise click.ClickException(str(e)) from e
