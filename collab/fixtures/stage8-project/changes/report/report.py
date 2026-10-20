import argparse
from slug import slug
from total import total


def render(title, values):
    return f"{slug(title)}:{total(values)}"


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("title")
    parser.add_argument("values", nargs="*")
    args = parser.parse_args()
    try:
        print(render(args.title, args.values))
    except ValueError:
        parser.error("values must be integers")
