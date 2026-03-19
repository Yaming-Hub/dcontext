# dcontext

## Problem Statement
When developing distributed application, lots of time the application want information to be captured and propogated across the call stack, so that the downstream method can get the information without via parameters. 

To support this, Rust language supports thread-local and Tokie async runtime has task-local variables, other async language may also have similar feature. However, there are limitations:
1. The thread-local and task-local variables are not exchange. There are cases when I span a thread without using tokio api in async function, the task-local variable won't be available in the new thread. Or, when I start a async runtime in a blocking thread, the task-local variable were not copied over either.
2. In the distributed system, when a call is made from one process to another, these contextual information is not propogated over.

## Solution
The idea is to build a distributed context library which:

1. It maintains a HashMap<string, any>, where the value is an any value can be downcasted to a specified struct. The struct should implement default and clone. The string key is the name of the contextual struct, it must be unique.
2. The application code need to register the context with concrete struct information.
3. The application code can get contextual information via static function like `let x = get_context::<MyStruct>("my_struct_key");`
4. The application code can set contextual information via static function like `set_context::("my_struct_key", value);`, when the contextual information is modified.
5. The context information is managed in a scope tree. Each time the context enters a new scope, all the modification of the context value will be visible to current scope. When the code leaves current scope back to it's parent scope, the modification in the child scope becomes invisible, all the context value will be remain its parent scope.
6. The struct value must implement seder trait, so that they can be serialized. The library cannot propogate the context to other process, but it provides help functions which serialize current context into bytes or string, and function to restore the context from deserialized bytes or string.
7. When calling from sync function to async or vise versa, the application code need to clone current context and begin a new scope in the new sync/async function call. Library provide help functions to make the code eaiser.
8. The library may consider provide macros to make application code eaiser to register or transfer context.
9. The library may provide extension functions to help start a new thread with current context.
10. The library should be flexible to support different async async runtime. Use cargo feature to configure.
