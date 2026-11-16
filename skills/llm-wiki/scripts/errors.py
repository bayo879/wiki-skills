"""Stable error type shared by the engine and its workspace helpers."""


class WikiError(Exception):
    """A failure with a stable code, an explanation, and a next action."""

    def __init__(self, code, message, next_step="", exit_code=1):
        super().__init__(message)
        self.code = code
        self.message = message
        self.next_step = next_step
        self.exit_code = exit_code
