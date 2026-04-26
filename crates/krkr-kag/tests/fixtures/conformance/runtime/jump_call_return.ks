; Expected result: call enters subroutine, return resumes after call.
*start
[call target=*sub]
after
[s]
*sub
inside
[return]
