Buildings are among the largest things people make together, and among the longest lasting: years of design, coordination, and argument, poured into something that goes on standing after everyone involved in it is gone. They are worth knowing about for that alone. They are also the best anchor a place has: long-lived, recognizable, and very unlikely to move. Find the same building in two photographs and you have tied two moments to one spot, whatever else changed around it. That combination is what makes the built environment one of the most powerful ways to contextualize history across time.

People already do this instinctively: sharing then-and-now photographs, exploring abandoned buildings, traveling specifically to walk through old cities and ruins. The curiosity is everywhere. What's been missing is a way to connect and explore it at scale. Chronoscope is building that.

Want to find old photographs of the building you're standing in front of? See what a demolished neighborhood looked like a century ago? Figure out what that crumbling ruin on the hillside actually was? Chronoscope connects places to their histories by pulling together photographs, maps, and records from sources across the web. AI models do the heavy lifting of analyzing millions of images, while a reasoning engine cross-checks everything and catches contradictions automatically. Anyone can contribute: resolve conflicts in the data, submit links to historical photos, or chase down mysteries nobody's solved yet.

Chronoscope doesn't host the images and records itself. Instead, it connects and makes sense of what's already out there: linking, analyzing, and organizing content from archives, photo collections, and public datasets into something searchable and browsable. The original sources keep their content and their rights. Chronoscope tracks connections between them.

## How it works

Chronoscope handles the messy reality of historical data. A date can be "circa 1920s" or "between 1914 and 1918." A location can be "somewhere in the Latin Quarter." Instead of forcing precision that isn't there, Chronoscope works with what it has and narrows things down as more evidence arrives.

Claims build on each other. An undated photograph of two unrecognized buildings tells you only that both were standing at the same moment, but that fact is worth keeping. When someone later identifies one of the buildings, the photograph inherits a date range from it, and the second building inherits part of its history in turn, without anyone having asserted anything about it directly. The same machinery that propagates knowledge this way is what notices when two sources can't both be right, and it re-runs over everything each time new evidence arrives. The [FAQ](/faq#how-does-chronoscope-work) walks through a worked example.

AI models work over each image in focused steps. A segmentation model isolates the individual buildings in a photo, drawing, or map. An embedding model indexes how each one looks, so a building in one image can surface candidates in others. A vision-language model reads what the image can tell us about them and how they sit relative to each other, such as one building standing next to another. None of those steps decides identity on its own: it all arrives at the reasoning engine as evidence, weighed alongside dates and locations, and that is where whether we have seen this building before is actually settled. Keeping the decision there rather than inside an image model makes the reasoning deterministic and traceable, with every conclusion pointing back to the specific evidence behind it.

Every claim in the system requires a citation. Citations are verified both by checking that the source actually says what's claimed, and by a separate AI model that evaluates whether the evidence supports the conclusion. This makes it hard for mistakes, misinformation, or deliberate disinformation to spread.

## Licensing

The Chronoscope codebase is open source under the MIT license. The knowledge graph content (the connections, analysis, and structured claims that Chronoscope produces and stores) is released under [Creative Commons Attribution 4.0](https://creativecommons.org/licenses/by/4.0/). The underlying photos, maps, and pages that Chronoscope cites retain their original licenses. We link to sources and cache images temporarily for analysis, but the rights to that content stay with their original creators.

## Contributing

The codebase includes the API server, analysis pipeline, iOS app, and this web frontend. Code contributions, ingestion pipelines, and research methodology improvements are all welcome.

Chronoscope is built by [Dan Peebles](https://github.com/copumpkin) and stewarded by the Chronoscope Foundation, formed to keep the project independent and accessible. The Foundation will accept donations. You'll be able to contribute the usual way, of course, but we're also cooking up some more creative ways to support the project. Stay tuned.

{{foundation_status}}
