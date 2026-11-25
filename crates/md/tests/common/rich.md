---
title: Rich
---

# Heading **one** #tag

Setext head
===========

Para with [[Note]], [[Note|Alias]], [[Note#Head]], [[#Only]], [[Note#^blk]] and ==mark== %%hidden%% :rocket: é日本😀
second line  
hard break ^blockid

- item one
- [ ] task one
  continuation
- [x] done
  1. nested ordered
  2. second

     para in item
  ```rust
  fn x() {}
  ```

3. three

> quote
> > nested
lazy

> [!NOTE]
> Alert body

> [!info] Custom title
> callout body

| a | b |
|:--|--:|
| 1 | 2 |

![[pic.png]]

![[Other Note#Sec]]

![alt](img.png "Title")

$$
x^2
$$

inline $a+b$ done

```math
e=mc^2
```

```typst
#let x = 1
```

    indented

<details>
<summary>Click</summary>

hidden

</details>

<div align="center">
<img src="logo.png" alt="Logo">
</div>

<!-- comment -->

<table><tr><td>x</td></tr></table>

Footnote[^1] and https://example.com and me@x.org <b>bold</b> <kbd>Ctrl</kbd><br>end

[^1]: The note

***

[ref]: http://x.y
[link][ref]

%%
block comment
%%

last
