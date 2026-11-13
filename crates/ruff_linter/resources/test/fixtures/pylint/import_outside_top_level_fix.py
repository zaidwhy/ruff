"""Module docstring."""

import sys
from typing import TYPE_CHECKING

pathlib = None


# Fixes


def simple():
    import os

    return os.getcwd()


def from_import_with_alias():
    from collections import OrderedDict as OD

    return OD()


def multiple_names():
    import json, csv

    return json, csv


def submodule():
    import xml.etree.ElementTree

    return xml.etree.ElementTree


def only_statement():
    from itertools import chain


class Class:
    def method(self):
        import base64

        return base64


def already_imported():
    import sys

    return sys.argv


# No fixes


def in_type_checking_block():
    if TYPE_CHECKING:
        import decimal


def in_try():
    try:
        import numpy
    except ImportError:
        numpy = None
    return numpy


def in_conditional(flag):
    if flag:
        import fractions


class ClassBody:
    import string


def relative():
    from . import sibling

    return sibling


def shadows_builtin():
    from io import open

    return open


def rebound_locally():
    import re

    re = None
    return re


def deleted_locally():
    import glob

    del glob


def conflicts_with_module_binding():
    import pathlib

    return pathlib


def outer():
    textwrap = None

    def inner():
        import textwrap

        return textwrap

    return inner, textwrap


def partially_imported():
    import sys, shutil

    return sys, shutil


def global_declaration():
    global heapq
    import heapq


if sys.version_info >= (3, 11):

    def in_conditional_function():
        import tomllib

        return tomllib
