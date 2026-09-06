"""Cross-file oracle: absolute import consumer."""

from mod_a import helper


def run():
    return helper(1)
