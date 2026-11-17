"""Stable error type shared by the engine and its workspace helpers."""

from pathlib import Path

# 不可自修的错误都指向随技能安装的操作手册；agent 用通用读文件工具打开 doc，
# 按 section 找小节。可自修的状态不报错，走 JSON 的 repaired 字段。
MANUAL_PATH = Path(__file__).resolve().parent.parent / "references" / "recovery.md"

MANUAL_SECTION = {
    "E_HEALTH": "运行时与安装",
    "E_STATE": "旧账本",
    "E_CONFIG": "配置损坏",
    "E_DB": "账本损坏",
    "E_PATH": "工作区边界",
}


class WikiError(Exception):
    """A failure with a stable code, an explanation, and a next action."""

    def __init__(self, code, message, next_step="", exit_code=1, section=None):
        super().__init__(message)
        self.code = code
        self.message = message
        self.next_step = next_step
        self.exit_code = exit_code
        self.section = MANUAL_SECTION.get(code, "") if section is None else section
        self.doc = str(MANUAL_PATH) if self.section else ""

    def as_dict(self):
        """The JSON error body: code/message/next, plus the manual pointer."""
        body = {"code": self.code, "message": self.message, "next": self.next_step}
        if self.doc:
            body["doc"] = self.doc
            body["section"] = self.section
        return body
