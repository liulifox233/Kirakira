; Expected result: macro expansion emits a ptext tag with text="hello".
[macro name=say]
[ptext text=%text]
[endmacro]
[say text="hello"]
