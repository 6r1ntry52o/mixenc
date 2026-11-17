## C/C++ code structure: the mixenc-analyst agent

When you need to know how the C/C++ code fits together - where something is defined, who calls a
function and what it calls, which classes derive from which, what a file declares - hand the
question to the mixenc-analyst agent instead of searching the code yourself. It asks clangd, reads
CP932 and other encodings right, and only reads.

- Give it: the question, and any symbol (Foo::Init), file or line you already know.
- It returns: a conclusion, the evidence as path:line, and what it did not check.
- To change a file afterwards, use the mixenc MCP server's edit tool (create for a new file): it
  keeps each file's encoding and every line you did not change. Built-in edit tools are stopped for
  files that are not UTF-8.
