from collections.abc import Awaitable
from typing import Any, Optional, TypedDict, Literal, TypeAlias, Union


class DecisionEvaluateOptions(TypedDict, total=False):
    max_depth: int
    trace: bool


class EvaluateResponse(TypedDict):
    performance: str
    result: dict
    trace: dict


ZenContext: TypeAlias = Union[str, bytes, dict]
ZenDecisionContentInput: TypeAlias = Union[str, ZenDecisionContent]


class ZenEngine:
    def __init__(self, options: Optional[dict] = None) -> None: ...

    def evaluate(self, key: str, context: ZenContext,
                 options: Optional[DecisionEvaluateOptions] = None) -> EvaluateResponse: ...

    def async_evaluate(self, key: str, context: ZenContext, options: Optional[DecisionEvaluateOptions] = None) -> \
            Awaitable[EvaluateResponse]: ...

    def create_decision(self, content: ZenDecisionContentInput) -> ZenDecision: ...

    def get_decision(self, key: str) -> ZenDecision: ...


class ZenDecisionContent:
    def __init__(self, decision_content: str) -> None: ...


class ZenDecision:
    def evaluate(self, context: ZenContext, options: Optional[DecisionEvaluateOptions] = None) -> EvaluateResponse: ...

    def async_evaluate(self, context: ZenContext, options: Optional[DecisionEvaluateOptions] = None) -> Awaitable[
        EvaluateResponse]: ...

    def validate(self) -> None: ...


def evaluate_expression(expression: str, context: Optional[ZenContext] = None) -> Any: ...


def evaluate_unary_expression(expression: str, context: ZenContext) -> bool: ...


def render_template(template: str, context: ZenContext) -> Any: ...


def compile_expression(expression: str) -> Expression: ...


def compile_unary_expression(expression: str) -> Expression: ...


class Expression:
    def evaluate(self, context: Optional[ZenContext] = None) -> Any: ...


def validate_expression(expression: str) -> Optional[ValidationResponse]: ...


def validate_unary_expression(expression: str) -> Optional[ValidationResponse]: ...


class ValidationResponse(TypedDict):
    type: Literal["lexerError", "parserError", "compilerError"]
    source: str
