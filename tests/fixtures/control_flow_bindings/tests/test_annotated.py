import src.annotated as annotated


def test_counter_starts_at_zero():
    assert annotated.COUNTER == 0


def test_headers_includes_content_type():
    assert "content-type" in annotated.HEADERS


def test_headers_length():
    assert len(annotated.HEADERS) == 2
