use std::{collections::HashMap, io::Write, ops::Range, path::Path};

use indexmap::IndexMap;

use crate::{Builder, Reader};
use bytes::Bytes;
use proptest::{
    arbitrary::Arbitrary,
    strategy::{BoxedStrategy, Just, MapInto, Strategy},
};
use tempfile::TempDir;

#[derive(Clone, Debug)]
pub struct TestEntryData(pub IndexMap<String, Bytes>);

/// Constructs TestEntryData from a format that is hopefully compatible with the
/// debug print output of this struct -- you can just copy-paste the debug output
/// in this macro.
///
///Only allows `&str` literals for the keys and `&'static [u8]` for the values.
///
/// ## Example
///
/// ```
/// use zippity::test_entry_data;
///
/// let data = test_entry_data!{
///     "entry1": b"some content",
///     "entry2": b"other content",
///     "entry3": b"and now for some completely different entry content",
/// };
/// assert_eq!(data.0["entry2"].as_ref(), b"other content".as_ref());
/// assert_eq!(data.0.len(), 3);
///
/// let reader: zippity::Reader<_> = data.into();
/// ```
#[macro_export]
macro_rules! test_entry_data {
    // This implementation is adapted from the indexmap::indexmap! macro.
    ($($key:literal: $value:literal),* $(,)?) => {
        {
            // Note: `stringify!($key)` is just here to consume the repetition,
            // but we throw away that string literal during constant evaluation.
            const CAP: usize = <[()]>::len(&[$({ stringify!($key); }),*]);
            let mut result = $crate::proptest::TestEntryData(indexmap::IndexMap::with_capacity(CAP));
            $(
                result.0.insert($key.into(), bytes::Bytes::from_static($value));
            )*
            result
        }
    };
}

/// A type that holds both a reader and a hash map with the entries the reader contains.
#[derive(Clone, Debug)]
pub struct ReaderAndData {
    pub reader: Reader<Bytes>,
    pub data: TestEntryData,
}

#[derive(Clone, Debug)]
pub struct ArbitraryTestEntryDataParams {
    /// How many entries to generate
    pub count_range: Range<usize>,

    /// How much data can an entry have
    pub max_size: usize,

    /// Use only simple printable nonempty ASCII strings for filenames
    pub entry_name_pattern: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct ArbitraryReaderParams {
    /// Parameters for generating the entries
    pub entries: ArbitraryTestEntryDataParams,
    /// Controls wheter the arbitrary implementation should also seek the reader to random position.
    pub seek: bool,
}

impl Arbitrary for TestEntryData {
    type Parameters = ArbitraryTestEntryDataParams;
    type Strategy = BoxedStrategy<TestEntryData>;

    fn arbitrary_with(args: Self::Parameters) -> Self::Strategy {
        let content_strategy = if args.max_size == 0 {
            Just(Bytes::new()).boxed()
        } else {
            proptest::collection::vec(proptest::bits::u8::ANY, 0..args.max_size)
                .prop_map_into::<Bytes>()
                .boxed()
        };

        proptest::collection::hash_map(args.entry_name_pattern, content_strategy, args.count_range)
            .prop_map_into()
            .boxed()
    }
}

impl Arbitrary for ReaderAndData {
    type Parameters = ArbitraryReaderParams;
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(args: Self::Parameters) -> Self::Strategy {
        let fresh_reader_strategy =
            TestEntryData::arbitrary_with(args.clone().into()).prop_map_into();

        if args.seek {
            fresh_reader_strategy
                .prop_flat_map(|reader_and_data: ReaderAndData| {
                    let len = reader_and_data.reader.size();
                    (Just(reader_and_data), 0..len)
                })
                .prop_map(|(mut reader_and_data, seek_offset)| {
                    reader_and_data.reader.seek_from_start_mut(seek_offset);
                    reader_and_data
                })
                .boxed()
        } else {
            fresh_reader_strategy.boxed()
        }
    }
}

impl Arbitrary for Reader<Bytes> {
    type Parameters = ArbitraryReaderParams;
    type Strategy = MapInto<BoxedStrategy<ReaderAndData>, Self>;

    fn arbitrary_with(args: Self::Parameters) -> Self::Strategy {
        ReaderAndData::arbitrary_with(args).prop_map_into()
    }
}

impl Default for ArbitraryTestEntryDataParams {
    fn default() -> Self {
        ArbitraryTestEntryDataParams {
            count_range: 0..64,
            max_size: 512,
            entry_name_pattern: ".{0,30}",
        }
    }
}

impl From<ReaderAndData> for Reader<Bytes> {
    fn from(value: ReaderAndData) -> Self {
        value.reader
    }
}

impl From<HashMap<String, Bytes>> for TestEntryData {
    fn from(value: HashMap<String, Bytes>) -> Self {
        TestEntryData(value.into_iter().collect())
    }
}

impl From<TestEntryData> for Builder<Bytes> {
    fn from(value: TestEntryData) -> Self {
        let mut builder: Builder<Bytes> = Builder::new();

        value.0.into_iter().for_each(|(name, content)| {
            builder
                .add_entry(name.clone(), content.clone())
                .expect("Adding entries from hash map should never fail");
        });

        builder
    }
}

impl From<TestEntryData> for Reader<Bytes> {
    fn from(value: TestEntryData) -> Self {
        Into::<Builder<Bytes>>::into(value).build()
    }
}

impl From<ArbitraryReaderParams> for ArbitraryTestEntryDataParams {
    fn from(value: ArbitraryReaderParams) -> Self {
        value.entries
    }
}

impl From<TestEntryData> for ReaderAndData {
    fn from(value: TestEntryData) -> Self {
        let builder: Builder<Bytes> = value.clone().into();

        ReaderAndData {
            reader: builder.build(),
            data: value,
        }
    }
}

impl TestEntryData {
    /// Creates a temporary directory that contains files corresponding to the test entry data.
    pub fn make_directory(&self) -> std::io::Result<TempDir> {
        let tmp = TempDir::new()?;

        for (name, value) in self.0.iter() {
            let path = tmp.as_ref().join(Path::new(&name));
            std::fs::create_dir_all(path.parent().unwrap())?;
            let mut f = std::fs::File::create(path)?;
            f.write_all(value)?;
        }

        Ok(tmp)
    }
}
