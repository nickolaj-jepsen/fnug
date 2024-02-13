from graphlib import TopologicalSorter

from fnug.config import ConfigCommand, Config


def schedule_commands(commands: list[ConfigCommand]) -> "TopologicalSorter[str]":
    """Create a graph of commands and their dependencies."""
    graph: TopologicalSorter[str] = TopologicalSorter()
    for command in commands:
        graph.add(command.id, *(dep.command.id for dep in command.dependencies))
    graph.prepare()
    return graph


def _get_active_commands_ids(commands_ids: list[str], all_commands: list[ConfigCommand]) -> list[str]:
    """Get the commands that are currently active (or dependencies of active commmands)."""
    result: set[str] = set()
    for command in all_commands:
        if command.id in commands_ids:
            result.add(command.id)
            result.update(_get_active_commands_ids([dep.command.id for dep in command.dependencies], all_commands))

    return list(result)


def get_active_commands(commands_ids: list[str], all_commands: list[ConfigCommand]) -> list[ConfigCommand]:
    """Get the commands that are currently active (or dependencies of active commmands)."""
    active_commands_ids = _get_active_commands_ids(commands_ids, all_commands)
    return [command for command in all_commands if command.id in active_commands_ids]


class Scheduler:
    def __init__(self, config: Config) -> None:
        self.map = self._dependency_map(config)

    def _dependency_map(self, config: Config) -> dict[str, ConfigCommand]:
        ...

    def run(self, commands_ids: list[str], rerun: bool = False) -> None:
        ...

    def stop(self, commands_ids: list[str]) -> None:
        ...

    def get_status(self, command_id: str) -> str:
        ...

    async def wait(self, command_id: str) -> None:
        ...
