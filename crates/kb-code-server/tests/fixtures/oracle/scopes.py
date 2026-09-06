# Oracle fixture: param shadow, comprehension isolation, nested defs (Python).

def value():
    return 1


def run(value):
    # ref `value` → param (not module fn)
    return value


def comp_isolate():
    x = 1
    ys = [x for x in range(3)]
    # ref `x` after comp → module-level assignment (comp target isolated)
    z = x
    return ys, z


def nested(x):
    def inner(x):
        # ref `x` → inner param
        return x
    return inner


def unbound_use():
    return missing_py
