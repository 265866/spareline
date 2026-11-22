import type { NumberView } from "./api";
import { formatNumber } from "./format";

// A query made only of digits and phone punctuation, such as "(512) 555" or
// "+1512", is also compared digit by digit, so any way of writing a number
// finds it.
const PHONE_LIKE = /^[\d\s()+.-]+$/;

const digitsOf = (value: string) => value.replace(/\D/g, "");

// True when `query` is a case-insensitive substring of the number in any
// format, its note, or the phone and SIM slot it is on.
export function matchesNumber(number: NumberView, query: string): boolean {
	const needle = query.trim().toLowerCase();
	if (needle === "") return true;
	const numbers = [number.e164, number.detectedE164].filter(
		(value): value is string => value !== null,
	);
	const fields = [
		...numbers,
		...numbers.map(formatNumber),
		number.e164 ? null : "Not detected",
		number.note,
		number.phone?.serial,
		number.phone?.model,
		number.phone ? `SIM ${number.phone.slot + 1}` : null,
	];
	if (fields.some((field) => field?.toLowerCase().includes(needle)))
		return true;
	const digits = digitsOf(needle);
	return (
		PHONE_LIKE.test(needle) &&
		digits !== "" &&
		numbers.some((value) => digitsOf(value).includes(digits))
	);
}

// The rows the Numbers page shows: the matches, plus every row whose note is
// open for editing, so neither a search typed mid-edit nor a refresh that
// changes the row can unmount the editor and lose its draft.
export function visibleNumbers(
	numbers: NumberView[],
	query: string,
	editing: ReadonlySet<number>,
): NumberView[] {
	return numbers.filter(
		(number) => editing.has(number.id) || matchesNumber(number, query),
	);
}
