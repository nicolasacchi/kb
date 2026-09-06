"""Hierarchy oracle: bases chain + duck-typed call (candidate)."""


class Base:
    def ping(self):
        return "base"


class Mid(Base):
    def ping(self):
        return "mid"


class Leaf(Mid):
    def run(self):
        helper()
        self.ping()


def helper():
    return 1


def use_duck(obj):
    # Duck-typed method call — must stay candidate.
    obj.ping()
